# Operator deployment

Choose one runbook. Do not combine their secret stores, configuration, or
provisioning backends.

- [Local, single-host runbook](operator-local-runbook.md): a source checkout,
  Docker Compose dependencies, filesystem secrets, and the Manager running on
  the host. Suitable for development and evaluation, not a public multi-tenant
  production deployment.
- [Production, AWS runbook](operator-production-runbook.md): separate staging
  and production environments, workload IAM, AWS Secrets Manager, and an
  isolated runtime backend. **The repository does not yet contain everything
  required for production go-live.** The runbook identifies the release gates
  rather than presenting a local Compose stack as a production installation.

The Manager commands in either environment are `setup`, `doctor`, and `serve`.
The tenant controller's `openflows doctor` is a different command.
