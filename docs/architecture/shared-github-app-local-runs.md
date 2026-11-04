# Shared GitHub App for SaaS and local runs

Status: approved product contract; connected-local implementation under review.

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

1. A contributor selects connected local mode during setup.
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

The existing self-managed App path remains available by explicit mode choice.
The connected-local rollout flag defaults off. Hosted SaaS workspaces keep
their current installation-token path, organization bindings, and Coder
provisioning behavior.

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
