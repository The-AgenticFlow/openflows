# Connected local mode: implementation and migration plan

Status: proposed plan for review; no code changes are authorized by this
document. Product behavior is specified in
[shared GitHub App for local runs](../../architecture/shared-github-app-local-runs.md).

## Delivery principles

Keep the existing SaaS Manager as the sole holder of the public GitHub App's
private key, OAuth client secret, and webhook secret. Add connected local mode
alongside the existing SaaS and self-managed local paths. Do not change local
setup's default until a complete private-repository smoke test passes. Each
work package should be independently reviewable and preserve the current
hosted broker behavior.

## Work packages

### L0 — Freeze the product contract

- Confirm that connected local mode uses the existing SaaS Manager and App,
  while Coder, the local Manager, and agents stay on the contributor's machine.
- Define personal development organizations as lightweight control-plane
  records, without automatic SaaS Coder provisioning. Support both personal
  and organization-owned installations through the existing authority policy:
  a GitHub organization owner and OpenFlows admin connects an organization
  installation; an invited member may enroll a local run only if organization
  policy permits it.
- Set local-run limits, session duration, inactivity expiry, and who may
  enroll/revoke a run. Default: a user manages their own runs; an organization
  admin can revoke runs using that organization's connection.
- Specify how local automation stops when hosted access is lost and how the
  user re-enrolls after session expiry.

Exit: the API, identity, ownership, and failure behavior are agreed; no
contributor-facing promise depends on an undecided policy.

### L1 — Data and authorization model

- Add a lightweight local organization or equivalent installation owner that
  satisfies the existing one-installation-to-one-organization invariant.
  Do not enqueue SaaS Coder provisioning for that kind.
- Add local-run records with immutable owner, organization, connection, repo,
  generation, expiry, revocation, and safe device label. Store enrollment and
  session secrets by hash; retain only encrypted references where recovery is
  required.
- Implement scoped repository queries that check active human membership,
  GitHub connection status, repository accessibility, and installation owner
  authority. Recheck at enrollment commit and credential issuance.

Exit: a direct API caller cannot enroll for another user's repository,
installation, or OpenFlows organization.

### L2 — Local-run enrollment and credential broker

- Add a human-authenticated enrollment endpoint using the existing CLI
  device-login session. Return a one-use, short-lived bootstrap once.
- Add registration, refresh, list, and revocation for local-run identities.
  Keep their audience and scope distinct from SaaS workspace credentials.
- Extend the GitHub broker with a local-run authorization path. Resolve
  installation, repository ID, role, and Git/API permission profile from
  trusted records. Recheck run and connection generations before and after
  GitHub exchange. Rate-limit by user and run.
- Stop affected local runs and revoke recorded leases on disconnect,
  suspension, repository removal, membership loss, and run revocation.

Exit: two users and two runs cannot cross repository boundaries; an in-flight
exchange cannot return a token after revocation.

### L3 — Local Manager and agent integration

- Add a connected-local configuration mode and a secure local credential
  store. The local Manager holds its run session; agents receive only the
  short-lived GitHub token for their role and repository.
- Add renewal during long jobs and fail-closed behavior on outage, expiry,
  permission mismatch, or revocation. Do not fall back to a personal token or
  another installation.
- Adapt local Git credential handling and GitHub API calls to the broker.
  Keep tokens out of Terraform state, process logs, checked-in environment
  files, and persistent agent prompts.
- Remove connected-mode dependence on Coder GitHub external auth from all
  five templates. Do not set the existing SaaS `hosted` flag merely to bypass
  external auth.

Exit: local clone/push, issue/PR operations, and token renewal work with a
  private test repository and no local App secret.

### L4 — Onboarding and compatibility

- Add a setup choice for connected local versus self-managed local. Make
  connected local the default only after L3 passes the end-to-end gate.
- Replace App registration in the connected path with hosted sign-in,
  install/connect, repository selection, and local-run enrollment.
- Update the quick start, manual setup, environment examples, doctor checks,
  and troubleshooting. Clearly state that the hosted service is required for
  fresh GitHub credentials.
- Keep the self-managed App path as an advanced option for independent
  deployments until its retirement is a separate explicit decision.

Exit: a new contributor can follow the documented path without creating a
  GitHub App, copying an App secret, or configuring a localhost callback.

### L5 — Release validation and rollout

- Run an isolated end-to-end smoke test with two personal GitHub accounts,
  separate private repositories, and separate local machines/containers.
- Check GitHub installation approval, callback replay, token scope, long-job
  renewal, one-run revocation, repo removal, App suspension/uninstall,
  membership loss, and hosted SaaS regression.
- Record test evidence and operational metrics: enrollment failures, broker
  denials, token exchanges, renewal failures, revocations, and webhook lag.
  Never log token values.
- Roll out first to the OpenFlows team, then opt-in contributors, then change
  the default setup path. Keep a rollback switch to the self-managed path
  without changing existing installations or distributing the central key.

Exit: every acceptance criterion in the specification has evidence, and
  hosted SaaS workspaces continue to use the shared App correctly.

## Migration order and dependencies

`L0 -> L1 -> L2 -> L3 -> L4 -> L5`. L1 and L2 add new local-run records and
routes without converting existing local installations. L3 opts a workspace
into connected mode. L4 changes onboarding only after the working path exists.
Existing local users may keep their own App until they explicitly switch;
switching involves hosted sign-in, installing the shared App on the selected
repository, enrolling a new local run, validating Git/API access, and only then
removing their old external-auth configuration if desired.

## Known implementation traps

- The current organization create operation provisions SaaS Coder resources;
  using it unchanged for every personal local tester would create unnecessary
  hosted infrastructure.
- The current runtime broker joins credentials to Manager-provisioned SaaS
  workspace rows. A local Coder workspace does not satisfy that trust chain.
- Local templates reference `coder_external_auth` even though hosted mode
  conditionally skips it. Connected local mode needs its own explicit path.
- Coder's GitHub external-auth OAuth exchange needs a client secret. The
  public App's client ID alone does not make that local flow usable; the central
  App secret must remain on the SaaS Manager.
- A single App registration has central setup and webhook destinations. Local
  machines must not register ad hoc callback/webhook URLs.
