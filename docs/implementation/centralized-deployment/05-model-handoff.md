# Implementation model handoff

Copy the following prompt into the implementation model's task. The specifications are repository files; provide the repository checkout with them.

---

Implement centralized Openflows deployment according to `docs/implementation/centralized-deployment/README.md` and its linked specifications. Start by reading all six documents and applicable repository instructions, then inspect current code and Git status. The older `docs/architecture/centralized-deployment-plan.md` is background; the detailed specifications take precedence where they refine it.

The product decisions are fixed:

1. Shared Coder Premium, one Coder organization per Openflows organization, managed behind Openflows.
2. Only active Openflows admins may initiate, complete, reconnect, or disconnect a GitHub App connection. Ownership is separate from membership role. Org creator starts as owner and admin.
3. One central GitHub App, installation-bound short-lived credentials scoped to the tenant repository and approved role permissions.
4. Manager is the product authorization and provisioning boundary; customers never need Redis or Coder operator credentials.
5. Coder hosts versioned templates; tenant/workspace creation selects explicit organization, owner, template, and approved version IDs.
6. Preserve existing agent orchestration, review gates, and explicit local mode. Hosted failures cannot fall back to privileged local bootstrap.

Work through WP-00 to WP-08 in `04-implementation-plan.md`, respecting dependencies. Use reviewable increments. Before each increment, state the scope and acceptance checks. Implement actual migrations, service logic, adapters, CLI integration, and tests; do not substitute scaffolding or TODOs for the required behavior.

Begin with WP-00 inventory/compatibility report and WP-01 database foundations. Current anchors are `crates/openflows-manager`, `binary/src/bin/agentflow.rs`, `crates/coder-client`, `crates/github/src/rest.rs`, `crates/config/src/identity.rs`, `crates/agent-nexus/src/lib.rs`, and all five role templates. The current manager has health routes, the CLI bootstraps Coder directly, and Nexus has default-org calls; these require deliberate migration.

Use PostgreSQL/SQLx for product metadata, durable operations, audit/outbox, and database constraints. Implement the API contracts and error semantics from the specs. Authentication, membership, installation authority, and runtime credentials are distinct concerns. Check org/resource scope before upstream calls. Never equate a visible GitHub installation with authority to administer it.

Do not copy platform Coder credentials, GitHub App keys, or global hook signing secrets into workspaces or Terraform parameters. Implement scoped secret injection, runtime registration/rotation, GitHub renewal, tenant Redis/network isolation, and trusted hook relay. If Coder cannot enforce a required runtime boundary, move that operation behind manager authorization; do not widen permissions.

Verify upstream API details against the pinned Coder version, initially v2.37.3 in Compose. Do not assume today's Coder documentation matches that version. If live credentials/license/backend access are missing, continue independent implementation with testable adapters and fixtures, document the blocked live checks, and report the exact operator input needed. Never claim those gates passed or fabricate production IDs/keys.

Run meaningful targeted tests for each increment, and integrated checks at the end. Include the acceptance matrix in your completion report with pass/fail/not-run and evidence. Preserve unrelated user changes. Do not deploy to production or migrate real customer data without explicit authorization. Commit/publish only if separately requested by the task owner.

At completion, provide:

- What was implemented and which work packages are complete.
- Migrations and configuration/operator setup required.
- Tests run and results, including live checks not run.
- Remaining blockers or deviations and their impact on deployment readiness.
- The next concrete work package if the implementation is incomplete.

---

For a bounded first implementation task, append: “This task covers WP-00 and WP-01 only. Deliver the compatibility report, database foundations, tests, and an updated backlog; do not claim the full deployment is implemented.”
