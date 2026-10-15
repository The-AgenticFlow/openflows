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

Complete transition graph (authoritative):

```
 planning -> plan_ready      (upload plan, set plan_ready)
 plan_ready -> building      (SENTINEL `gate decide approve` — atomic)
 plan_ready -> plan_rejected (SENTINEL `gate decide reject`)
 plan_rejected -> planning   (FORGE revises and resubmits)
 building -> testing         (FORGE commits, clean checkout, set testing)
 testing -> submit           (A2A verified + SENTINEL testing approve — atomic)
 testing -> building         (SENTINEL/human testing reject, or FORGE rework)
 submit -> building          (SENTINEL/human PR reject, or FORGE rework)
 submit -> done              (VESSEL: CI + SENTINEL + human PR approve + merge)
 any phase -> blocked        (external prerequisite only)
 blocked -> planning         (FORGE only, once blocker clears; NO direct blocked->building)
 done                        (terminal — no exit)
```

Rework always returns through `building`, then repeats `testing` and both review
gates; never jump from `building` directly to `submit`. There is no path from
`blocked` straight to `building` — recovery returns through `planning` and a
fresh plan approval. NEXUS re-awakens FORGE when a ticket is blocked so it can
resume autonomously once the blocker clears.

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

SENTINEL reads `sentinel-testing` and derives checks from the exact approved
plan DoD before execution. Reconsider irrelevant commands before asking FORGE
to install tools. A rejected necessary test/command/setup failure returns FORGE
to `building` under the approved plan to repair code or its environment. Record
the unverified criterion and diagnostics; do not claim tests ran when they could
not start. Missing dependencies for necessary checks are repairable workspace
setup failures, not external blockers. Inspection-only evidence cannot currently
satisfy the executable A2A gate: report that limitation and request clarification
or an authorized plan revision, without meaningless commands or a gate bypass.
Use `blocked` only for a genuinely **external** prerequisite outside
FORGE's workspace that FORGE cannot resolve by installing/repairing its own
environment (e.g. missing human approval, unreachable external service,
credentials/secret unavailable to the project), and record exact evidence and an
answerable unblock question. Recovery returns to planning
through the authorized `blocked -> planning` transition (FORGE only) — there is
no direct `blocked -> building`. Do not retry configuration failures or policy
denials in a loop, or delegate denied work to bypass the hook. A denied
read-only planning probe should be reported to NEXUS with the command and
rejection.

After submitting for review, wait for the orchestrator notification without
polling loops. Retry only genuinely transient transport failures with bounded
backoff. Keep handoffs in the harness with `handoff write --contract <file>
--notes <notes>`. Never dump credentials or secrets into coordination artifacts.
