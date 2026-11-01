# Coder Premium requirements for OpenFlows

OpenFlows' current organization-per-customer deployment design depends on Coder capabilities that are not available in a Community-only deployment. This is a deployment requirement, not a requirement of the shared OpenFlows GitHub App or GitHub device sign-in.

## Blocker observed in local testing

On 2 November 2026, the local Coder deployment rejected an OpenFlows-managed provisioner daemon with HTTP 403:

> External provisioner daemons is an Enterprise feature. Contact sales!

The daemon container was running, but could not connect to Coder and take jobs. A `template_version_import` job remained `pending`; OpenFlows' `template.promote` operation reached `publish_templates`, timed out with `IMPORT_TIMEOUT`, and retried. Pending workspace builds can also delay tenant creation and deletion. A running Docker container or a healthy Manager HTTP endpoint does not prove that Coder has an available provisioner.

Coder describes [external provisioners](https://coder.com/docs/install/operate/provisioners) as separate daemons that perform template and workspace builds. Its [plan comparison](https://coder.com/pricing) lists external provisioners under Premium. OpenFlows creates an organization-scoped provisioner key and daemon for each customer organization; see the [Manager deployment notes](../crates/openflows-manager/README.md) and the [compatibility report](implementation/centralized-deployment/compatibility-report.md).

## Why the current design needs Premium

| Coder capability | OpenFlows use | Without it |
| --- | --- | --- |
| External, organization-scoped provisioner daemons | A dedicated daemon takes each organization's template imports and workspace builds. | Jobs stay pending or fail, so template promotion and workspace lifecycle operations cannot finish. This is the feature gate observed locally. |
| Multi-organization access control | Customer resources and roles are assigned to distinct Coder organizations. | The current organization-per-customer isolation model cannot be assumed to work as designed. |
| Service accounts and organization roles | Manager creates and assigns narrowly scoped machine identities during organization provisioning. | Automated provisioning may be rejected or require a different identity model. |

Coder lists [multi-organization access control and external provisioners](https://coder.com/pricing) in Premium; its [roles documentation](https://coder.com/docs/admin/users/groups-roles/) describes organization roles and service accounts. Exact entitlements should be verified against the license installed on the deployment and the Coder version being run. GitHub App installation, device authorization, and the Manager's browser login are separate from this Coder licensing gate.

## Deployment and test guidance

1. For the existing organization-per-customer architecture, use a Coder deployment licensed for the required Premium capabilities. Confirm that an organization provisioner reaches `idle` or `busy` before promoting templates or creating workspaces.
2. Check Coder's provisioner daemon status and job queue when an OpenFlows operation stays in `publish_templates` or `waiting_for_nexus`. A pending `template_version_import` with no eligible daemon is an infrastructure failure; retrying the OpenFlows operation alone will not fix it.
3. Keep `OPENFLOWS_PROVISIONER_SECRET_DIR` durable and at the same absolute path when running Manager from another worktree. A changed path can make existing Docker provisioners fail OpenFlows' identity and mount validation even when the license is valid.
4. A Community-only local trial would need an explicitly designed single-organization path using Coder's built-in provisioner, with different isolation and identity assumptions. The current Manager does not provide that fallback. Do not present the existing multi-organization flow as supported on Community merely because the containers start.

The [Coder licensing guide](https://coder.com/docs/install/prepare/licensing/) explains how licenses are applied. Do not put a license key or provisioner credential in the repository or this document.
