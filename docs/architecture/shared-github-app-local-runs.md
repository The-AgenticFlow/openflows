# Shared GitHub App for SaaS and local runs

Status: implemented for local runs; hosted installation-token behavior remains separate.

## Two credential paths

The OpenFlows team owns one public GitHub App. Hosted SaaS automation retains
its existing installation-token broker and server-side App private key.
Connected local runs act on behalf of the contributor with a GitHub App user
access token. GitHub limits that token to permissions and repositories shared
by the user and the App installation. A contributor whose GitHub access is
removed must lose the corresponding ability to operate locally.

The local Manager never receives the App private key or client secret. It
uses the public App client ID and GitHub's device authorization flow. The App
must have device flow enabled. Request `offline_access` to obtain an expiring
access token and a rotating refresh token. GitHub permits refresh of a token
obtained through device flow without the App client secret, so both sign-in
and renewal happen directly between the local Manager and GitHub.

## Local journey

1. Setup starts the local Manager with the shared App configuration.
2. The local Manager requests a device code from GitHub and displays the
   verification address and user code. The contributor authorizes the App.
3. The Manager polls within GitHub's required interval and stores the returned
   user access and refresh tokens in secure local credential storage.
4. It lists installations and repositories visible to that user token, then
   verifies each requested repository by immutable GitHub repository ID. GitHub remains the
   authorization authority for subsequent Git and API calls.
5. Before expiry, the Manager refreshes directly with GitHub and atomically
   replaces both tokens. Concurrent jobs share one renewal operation.
6. Revocation, expiry, or lost access stops operations and prompts for a new
   device authorization where appropriate.

Device authorization is distinct from App installation. A repository owner
still installs the App on the intended repository in GitHub. GitHub handles
which accounts and repositories that owner may select.

## Storage and isolation

No per-device local-run record, personal OpenFlows organization, hosted
enrollment, or hosted refresh endpoint is required. Local credentials must
never enter checked-in files, Terraform state, logs, prompts, or SaaS runtime
tables. Sign-out removes the local credentials. If GitHub returns a rotated
refresh token, persist it atomically before another refresh can begin.

Hosted SaaS workspaces keep their installation-token path, organization
bindings, and Coder provisioning behavior.

## Operator rollout and recovery

The supported local setup uses the shared OpenFlows App. In the App's GitHub
settings, enable device flow and expiring user tokens, and grant repository
access only to the repositories intended for OpenFlows. The local release
needs the public App client ID and slug; the App private key and client secret
stay on the hosted deployment. Contributors run `./scripts/setup.sh`, install
the App on their repository, connect GitHub in the local console, and select
that repository. A local Coder account is still needed for workspaces.

To recover from expired or revoked authorization, use **Connect GitHub** in
the local console. If a repository disappears, verify both the user's GitHub
access and the App installation's repository selection, then refresh the
console. If a worker failed to start, restart its Coder workspace build after
restoring access. A Manager restart preserves the local credential file; a
new device authorization is needed if the refresh token is no longer valid.

**Disconnect** deletes the Manager's saved credential and prevents new broker
requests. Running workspaces may retain an already issued access token until
GitHub expires or revokes it; the workspace helper clears its cached token
when the Manager explicitly denies renewal, with checks once per minute.
For immediate invalidation of an issued token, revoke the App authorization
in GitHub. Restricting user access or removing the App from a repository makes
GitHub deny that repository's subsequent operations.

For rollback, stop the local Manager and workspaces, back up the local
credential file outside the repository, then restore the previous release and
its matching Coder templates. Reconnect GitHub if the restored release cannot
read the saved credential format. Check hosted SaaS separately; its App
installation-token path is independent of local user authorization.

Before declaring a rollout complete, record a live run for two GitHub users
with different private repositories: device authorization, Git push and API
actions, renewal across a long job, Manager restart, sign-out, revoked user
authorization, user access removal, App repository removal, and a hosted SaaS
regression. Record outcomes and configuration without tokens or private data.

## Acceptance

- A new contributor can run locally against an approved private repository
  without creating an App or receiving an App secret.
- Two users cannot operate on each other's private repositories unless GitHub
  independently grants the user and App access.
- Local Git push and issue/PR operations use the user token; renewal works
  during long jobs and after local restart.
- Removing user access, App repository access, or user authorization prevents
  future GitHub operations.
- SaaS onboarding and hosted workspace GitHub operations remain functional.

GitHub references: [device authorization](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app#using-the-device-flow-to-generate-a-user-access-token), [user token access limits](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app#about-user-access-tokens), and [device-flow refresh](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/refreshing-user-access-tokens#refreshing-a-user-access-token-with-a-refresh-token).
