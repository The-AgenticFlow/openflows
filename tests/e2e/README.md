# OpenFlows container CI gate

CI exercises OpenFlows against disposable Redis, PostgreSQL, and Coder containers before the final `OpenFlows Merge Gate` can pass. PRs into `develop` and `main`, pushes to those branches, and merge queue groups run the gate. Every mandatory dependency must explicitly succeed; failed, cancelled, and skipped jobs fail the gate.

See the [complete E2E testing architecture](../../docs/architecture/end-to-end-testing.md) for the purpose of PRs #400 and #401, the target issue-to-merge system, and the remaining implementation stages.

## Run locally

Requirements: Linux x86_64, Rust, Docker Engine with Compose, GNU `timeout`, `tar`, and access to container registries and Terraform provider downloads. The Coder test extracts its CLI from the pinned server image. No GitHub credentials or model API keys are needed.

```bash
bash tests/integration/gated_workflow_test.sh
bash tests/e2e/run.sh
OPENFLOWS_E2E_SUITE=verification bash tests/e2e/run.sh
bash crates/openflows-manager/scripts/run-integration-tests.sh
bash tests/e2e/system/run.sh
```

The Coder runner builds the current harness binary into an Ubuntu 24.04 worker image. Coder uploads a Terraform fixture, provisions a real worker, and the Rust client executes the harness through real Coder SSH. The test rejects self-approval, a rejected plan, and a stale review; accepts the current reviewed plan; checks a second tenant; stops and starts the workspace; verifies lifecycle persistence; and deletes the workspace.

The Redis runner covers atomic competing decisions, write failure under Redis OOM, review leases, a persisted planning-to-completion lifecycle, missing verification evidence, changed commit identity, client reconnection, and tenant separation. OOM changes the server's global configuration, so this suite runs serially.

The delegated-verification suite provisions separate FORGE and SENTINEL workers. It starts the production A2A HTTP relay against real Redis and runs the actual harness executor in FORGE. A committed acceptance script requires the answer `42`; the first candidate contains `41`. SENTINEL requests verification, observes the failing exit code, and cannot approve testing. FORGE fixes and commits the answer, then SENTINEL must obtain successful verification for that new commit. Approval for the older head/round is rejected. The current verified candidate can reach submit, and the original FORGE checkout must remain clean. The relay fixture seeds only the pair authentication token, never verification results or lifecycle transitions.

The [system fixture contracts](system/README.md) provide a scripted model HTTP server, a limited GitHub API adapter backed by real Git, and an independent exact-commit acceptance runner. They exercise the production GitHub client and run fixture contracts in an isolated container. They are task 3 infrastructure; the production controller and real Coder chats are not connected yet.

The existing manager suite uses real PostgreSQL, creates isolated test databases, and applies migrations. CI supplies its own PostgreSQL service.

## Isolation and diagnostics

The Coder runner uses a unique Compose project, loopback ports allocated by Docker, no operator `.env`, and no production data volumes. Worker containers carry a run-specific label. Exit cleanup collects logs, removes only those workers, and tears down the test stack. Redis fault injection always targets a separately created disposable server.

Artifacts are saved beneath `target/ci-artifacts/` and uploaded on CI success or failure. The Coder test has a 20-minute deadline and its CI job has a 35-minute deadline. Run these tests only on disposable runners: Coder's Terraform provisioner needs the Docker socket to create worker containers.

## Enable merge enforcement

After the workflow has run successfully in GitHub, configure repository rulesets or branch protection for both `develop` and `main`:

1. Require pull requests and `OpenFlows Merge Gate` from GitHub Actions.
2. Require a merge queue where available, or require the branch to be up to date before merging.
3. Apply the rules to automation accounts as well as developers. Review bypass permissions explicitly.
4. Require review for changes to CI workflows and test fixtures, using the project's code owners.

A workflow file alone does not enforce merge blocking. These repository settings must be applied separately. GitHub accepts skipped status checks as passing, so the final gate uses `always()` and validates each dependency's result. See [GitHub required status checks](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks).

## Coverage boundary

This is a real container integration gate and a Coder worker E2E suite. The worker uses a dedicated CI template: it does not validate the production FORGE template's GitHub external-auth bootstrap. The persisted lifecycle test invokes production transition APIs directly; its verification, PR, and merge events are controlled inputs, not actual GitHub actions.

A complete issue-to-merged-PR system suite still needs a scripted model/provider, the running NEXUS controller and role orchestration, real Coder lifecycle hooks, and a GitHub fixture that independently verifies requests and commit identity. The delegated-verification suite covers the A2A transport and execution boundary, but its driver dispatches commands through Coder SSH rather than model tool calls. A separate credentialed smoke suite must exercise the production templates, real GitHub repository, and actual model provider. Neither is represented as complete by the container gate.

Coder's lifecycle-hook flags are hidden from `server --help`; validate them by supplying the flags rather than searching the help output. Do not treat manually submitted hook envelopes as proof of Coder-to-controller hook integration.
