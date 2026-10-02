---
name: shared-harness-protocol
description: Use when assigned agents coordinate ticket state, plan review, verification, PR review, or recovery through openflows-harness.
---

# Shared Harness Protocol

## Authoritative state

Before writing a plan, inspect relevant source files, repository configuration,
and runtime capabilities read-only. This is allowed before approval; source
edits and runtime mutations remain gated.

Use `openflows-harness status get`, `dispatch read`, and `plan read` for the
current lifecycle, requirements, and plan. Shared state belongs to the harness;
local status, contract, and segment-review files are not coordination signals.
Read state on assignment, resume, and approval notifications. Use the current
revision, review round, and head for decisions; stale messages do not authorize
work in a newer phase. SENTINEL reviews plans; it does not write FORGE's plan.

## Plan file location

Use Coder's current chat plan path, `/home/coder/.coder/plans/PLAN-<chat-id>.md`,
as supplied by Coder or the startup hook. Replace `<absolute-plan-path>` in
commands with that exact path. Do not write `/home/coder/PLAN.md` or copy the
plan into source just to upload it; the harness reads the supplied path directly.

## Lifecycle

`planning -> plan_ready -> building -> testing -> submit -> done`

- **Planning:** FORGE reads source, tests, repository metadata, deployment
  configuration, and runtime capabilities before writing a grounded plan.
  Read-only inspection needs no approved plan. Writing the plan and coordination
  artifacts is allowed; source edits and runtime mutations remain gated.
- **Plan review:** FORGE uploads with `plan write --file <absolute-plan-path>`, then sets
  `plan_ready`. SENTINEL reads the stored plan and decides with
  `gate decide --phase plan_ready --revision <N> --round <R> --verdict
  approve|reject --report review.md`.
- **Approval:** SENTINEL approval atomically enters `building` in shared state.
  FORGE confirms that current phase with `status get` before implementation;
  no separate FORGE transition is needed. Rejection enters `plan_rejected`;
  FORGE returns to `planning`, revises, and resubmits.
- **Testing:** FORGE verifies and commits all implementation changes, sets
  `testing`, keeps the checkout clean, and runs `verify serve`. SENTINEL requests
  tests with `verify request --expect-exit 0 -- <program> <arguments>` and
  decides using `gate decide --phase testing --revision <N> --round <R>
  --head <SHA> --verdict approve|reject --report review.md`. Successful A2A verification and SENTINEL approval are required for the current
  head. TODO(human-testing-review): add human approval later; it does not block submit now.
- **Submit:** After testing approvals, FORGE sets `submit`, opens or updates and
  records the PR. Read the current round again after PR recording. SENTINEL uses
  `review submit --revision <N> --round <R> --head <SHA> --verdict
  approve|reject --report final-review.md`. Human PR approval is also required.
- **Done:** VESSEL requires current-head CI success, SENTINEL and human PR
  approval, and confirmed merge. Missing or timed-out CI never counts as success.

Humans record testing/submit decisions using the operator CLI:
`openflows gate decide --tenant <tenant> --ticket <ticket> --phase testing|submit
--revision <N> --round <R> --head <SHA> --verdict approve|reject --notes <reason>`.

## Rework and blockers

Testing and submit freeze source. Rework returns through the permitted
`building` transition, then repeats testing and both review gates; never jump
from building directly to submit. An old approval cannot override `blocked`.

Use `blocked` for an operational failure and record exact evidence and an
answerable unblock question. Recovery returns to planning through the
authorized lifecycle. Do not retry configuration failures or policy denials in
a loop, or delegate denied work to bypass the hook. A denied read-only planning
probe should be reported to NEXUS with the command and rejection.

After submitting for review, wait for the orchestrator notification without
polling loops. Retry only genuinely transient transport failures with bounded
backoff. Keep handoffs in the harness with `handoff write --contract <file>
--notes <notes>`. Never dump credentials or secrets into coordination artifacts.

During testing SENTINEL may use `status set blocked` for an infrastructure or
verification-policy blocker. Record the error and missing evidence in review.md
and stop; reserve rejection/rework for actionable code defects. FORGE recovers
from blocked through planning after the infrastructure is repaired.
