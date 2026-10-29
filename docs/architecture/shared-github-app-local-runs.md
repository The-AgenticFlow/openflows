# Shared GitHub App for SaaS and connected local runs

Status: proposed specification for review. No implementation is implied by
this document.

## Goal and deployment boundary

One public, OpenFlows-owned GitHub App serves both the OpenFlows SaaS and
contributors running OpenFlows from source. The existing SaaS Manager remains
the App's trusted authority: it holds the private key, OAuth client secret,
and webhook secret, receives GitHub callbacks and webhooks, verifies
installations, and mints installation tokens. A contributor's Manager and
agents run locally and call the SaaS Manager for GitHub credentials. They never
receive the App key or OAuth client secret.

This is **connected local mode**. It requires the SaaS Manager to be reachable
for enrollment and fresh GitHub tokens. An independently operated/offline
OpenFlows deployment remains a separate mode with its own credentials. SaaS
and connected local mode share the App registration, but not runtime identities
or workspace ownership.

## Current state and gap

- The hosted Manager already implements GitHub App connection verification,
  webhook ingress, App JWT signing, scoped installation-token exchange, and a
  broker for Manager-provisioned SaaS workspaces.
- `scripts/setup.sh` currently creates a new GitHub App and places its client
  ID and secret in local `.env`. Local Coder external auth uses that secret
  for OAuth and the five local templates consume its user token.
- Hosted templates use Manager runtime credentials and the broker. Their
  `hosted` parameter is coupled to SaaS provisioning; setting it on a local
  workspace does not create an authorized local identity.
- The broker's current runtime scope joins a credential to a SaaS workspace,
  tenant, organization, connection, and repository. Connected local runs need
  a distinct scope and lifecycle.

## User journey

1. The contributor starts local setup and chooses connected local mode.
2. Local setup starts the existing OpenFlows CLI device-login flow against the
   SaaS Manager. The user signs in with GitHub in the hosted browser flow.
   This proves human identity; it does not itself grant repository access.
3. The user connects the shared OpenFlows App to a GitHub account they own or
   administer, selecting the intended repository in GitHub's installation UI.
   The hosted Manager completes its existing OAuth/setup-state and authority
   checks, then records the installation and accessible repository IDs.
4. Local setup asks the SaaS Manager to enroll this local run for one accessible
   repository. The Manager derives the OpenFlows organization and connection
   from the signed-in user and verified installation. It returns a one-use,
   short-lived enrollment secret to the local Manager over TLS.
5. The local Manager exchanges that secret for a run session and requests
   short-lived, repository- and role-scoped GitHub tokens for its agents. It
   renews tokens before expiry. The existing 24-hour hosted runtime refresh
   family should not be assumed sufficient for unattended local runs; define
   explicit local-run session renewal through the user's authorization.
6. The user can list and revoke their local runs. Revocation, membership loss,
   repository removal, installation suspension/uninstall, or connection
   disconnect denies future issuance and stops affected local automation.

GitHub controls which accounts a user may install the App on. The OpenFlows
picker must display only repositories the hosted Manager has verified for that
user. The server must enforce the same rule on every API request; a crafted
request cannot select another organization, installation, or repository.

## Identity and ownership model

A **local run** is a new principal type, distinct from a human session and a
SaaS workspace runtime. Persist its immutable run ID, enrolling user, OpenFlows
organization, GitHub connection, immutable repository ID, device label,
creation/last-seen/expiry/revocation timestamps, and credential generation.
The device label is display-only and never an authorization input.

The existing rule that one GitHub installation binds to at most one OpenFlows
organization remains. It is an internal relationship, not a choice among all
customer organizations. A personal installation belongs to the user's
personal development organization. The current organization-creation path
automatically provisions SaaS Coder resources, so connected local mode needs
a lightweight organization kind or an equivalent connection owner that does
not trigger SaaS provisioning. An organization installation requires the
existing GitHub owner and OpenFlows admin verification. A local run inherits
the verified binding. This prevents a local test run from taking ownership of
an installation already bound elsewhere.

Enrollment should default to repositories in a contributor's own installation.
Running against a repository in a shared organization requires an explicit
organization policy and authorized member; it is not granted merely because a
user has a local checkout. A non-member of the OpenFlows team's GitHub and
OpenFlows organizations receives no team repository option or token.

The hosted Manager derives each token's installation, repository, and
permission profile from persisted local-run identity and the requested agent
role/purpose. The caller may request a coarse purpose such as Git or API, but
cannot supply a permission map or override organization/repository scope.

## Credential and API contract

The following routes are proposed; names may be adjusted during implementation.

| Operation | Caller | Required behavior |
| --- | --- | --- |
| Create local run | Authenticated human CLI | Verify membership, installation, repository, local-run policy; return one-use enrollment secret once |
| Register run | Local Manager with enrollment secret | Atomically consume secret; issue scoped access and refresh session |
| Get GitHub credential | Local Manager with run access token | Resolve role and purpose server-side; return short-lived installation token with no-store response |
| Refresh run session | Local Manager with refresh token | Rotate/revoke on reuse; enforce run and connection generations |
| List/revoke runs | Authenticated owner or authorized admin | Show safe metadata; revoke one run without affecting other runs |

The local Manager brokers tokens to its own agents; it does not hand them its
central run refresh credential. Agent tokens are limited to the selected
repository and approved role permissions. Keep raw credentials out of
Terraform state, checked-in `.env` files, logs, and list responses. Local
temporary credential files use restrictive permissions and are removed on
stop/revocation.

Authorization is rechecked before and after GitHub token exchange. Cache
entries and leases include connection and run generations. A revoked run or
changed installation cannot receive a cached or in-flight token. Already
issued GitHub installation tokens may remain usable briefly; stopping local
automation and revoking recorded leases reduces that window.

## Local Coder and setup behavior

Local Coder continues managing workspaces but no longer performs GitHub App
OAuth in connected local mode. Its local user login is independent of the
hosted OpenFlows identity. Local templates obtain Git/API tokens through the
local Manager's brokered runtime path, with renewal during long jobs. Remove
the connected-mode dependency on `coder_external_auth` before deleting the
legacy App-creation step. Preserve self-managed App mode as an explicit
advanced path until connected mode passes the release checks.

## GitHub registration and operations

Configure the one public App with the SaaS Manager's OAuth callbacks, setup
URL, and webhook URL. Do not register arbitrary localhost callback URLs or
distribute App secrets. The setup callback's installation ID is untrusted until
the Manager verifies the authenticated user's authority and App ownership.
Permission grants on the App are the maximum; a repository owner may need to
approve later permission changes before a role's token profile works.

Use the same App registration for SaaS and connected local mode. Separate
staging App credentials may be useful for release testing, but staging must
not be silently substituted for the production App in contributor setup.

## Acceptance criteria

- A new contributor completes setup against a private personal test repo
  without creating an App, receiving an App secret, or configuring a localhost
  GitHub callback.
- Two users and two local runs cannot exchange repository scopes by changing
  request bodies, IDs, or cached credentials.
- Local clone/push and GitHub issue/PR calls work, and token renewal succeeds
  during a long-running job.
- Revoking one local run leaves another run and SaaS workspaces unaffected.
- Repository removal, installation suspension/uninstall, user or membership
  removal, and connection disconnect stop new token issuance and stop the
  affected local run.
- SaaS onboarding and hosted workspace GitHub access remain functional with
  the same App.
