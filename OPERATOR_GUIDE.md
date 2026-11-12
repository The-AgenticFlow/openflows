# OpenFlows operator guide

For a local install, follow the [quick start](quick_start.md). The local
Manager handles the shared OpenFlows App, GitHub user authorization, Coder
workspaces, and tenant operations. Operators do not configure Coder GitHub
external auth or distribute an App private key to local contributors.

For hosted deployments, follow the
[operator deployment guide](docs/implementation/centralized-deployment/operator-deployment.md)
and the [production runbook](docs/implementation/centralized-deployment/operator-production-runbook.md).
The hosted Manager owns App installation credentials and brokers scoped
installation tokens to runtimes. Hosted provisioning and local device
authorization are separate Manager flows.

## Monitor

Use the Manager console for organization, tenant, workspace, and operation
status. Check Coder workspace build logs for agent startup failures. A
healthy worker registers with the Manager, obtains its GitHub credential,
and sends a readiness heartbeat.

See the [token guide](token_guide.md) for local credential handling and the
[Manager runtime architecture](docs/architecture/manager-hosted-runtime-architecture.md)
for the hosted credential contract.
