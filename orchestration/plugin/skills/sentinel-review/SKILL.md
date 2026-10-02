---
name: sentinel-review
description: Use when SENTINEL reviews an exact plan revision, verifies implementation through A2A, or reviews a submitted PR.
---

# SENTINEL Review Skill

Read `openflows-harness status get`, `dispatch read`, and `plan read` first.
Review the current phase, revision, review round, and candidate head. Do not
restart planning, modify source, or rely on local contract/segment files.
This ticket uses one SENTINEL conversation across plan revisions, testing, and
PR review. Each follow-up is a new review of the stated revision, round, and
head; preserve prior findings as context, but never reuse an old approval or
test result as evidence for a new candidate.

## Testing: verify the approved plan through FORGE

Infrastructure failure is not a code-review rejection. If the relay is unreachable,
returns an internal error, or a task is never claimed/completed, write the exact
command, error/task ID, and missing evidence to `review.md`, run
`openflows-harness status set blocked`, and stop. Do not use `gate decide --verdict
reject` to restart building for an infrastructure-only failure. NEXUS/operator
must resolve the blocker before FORGE recovers through planning. A healthy relay
or advertised capability does not prove an executor is available. Submission
errors alone do not prove the executor is absent: tasks are queued before claim.
For a command-policy rejection, inspect the specific denied operation. Correct
argument mistakes; do not cycle through unrelated commands. Report real policy
or workspace toolchain blockers and stop.
Project-specific verification commands are allowed in the temporary FORGE checkout. Known
destructive/control-plane operations have explicit denials. `echo hello` checks
transport only and never proves acceptance criteria. Missing toolchains or offline
dependencies require the operator to prepare the existing FORGE workspace/toolchain
and retry verification.


FORGE commits the implementation, enters `testing` with a clean checkout, and
runs `openflows-harness verify serve`. SENTINEL sends commands through A2A:

```bash
openflows-harness verify request --expect-exit 0 -- cargo test --workspace --all-features
```

Pass the executable and each argument as separate tokens after `--`. Never use
`--argv "cargo test"`: that sends one executable token and is a syntax mistake,
not evidence that Cargo is disallowed. The legacy form requires one flag per
token: `--argv cargo --argv test --argv=--workspace --argv=--all-features`.
Correct a tokenization error and retry the intended command before classifying
a policy blocker. Harness options such as `--timeout-secs` go before `--`.
Arguments after it are passed literally; shell operators and expansions are not executed.
For frontend verification use `-- npm run build`, `-- npm run lint`, or
`-- npm run test:unit` as appropriate for the repository's scripts.

NEXUS routes the request to FORGE's executor. Read the returned task result,
exit code, output, timeout status, and candidate head. Run project-appropriate
tests and checks that establish the approved plan's acceptance criteria.
Do not substitute tests in SENTINEL's own checkout for A2A verification in
FORGE. A trivial successful command alone does not demonstrate plan completion.

Write `review.md` mapping each acceptance criterion to the relevant code,
A2A task/command, observed result, and any gap. Include actionable file/line
feedback and required fixes. Do not claim success for missing, timed-out,
failed, or stale-head evidence. If FORGE's executor is unavailable, report the
specific infrastructure blocker; do not approve or repeatedly spawn reviewers.

After all criteria are satisfied, record the exact current decision:

```bash
openflows-harness gate decide --phase testing --revision <N> --round <R> --head <SHA> --verdict approve --report review.md
```

Use `--verdict reject` when changes are required; rejection returns FORGE to
`building`, and testing must be repeated for the new candidate. Successful
A2A verification plus SENTINEL approval permits FORGE to enter `submit`.
TODO(human-testing-review): add human approval before submit later. For now,
do not wait for it or fabricate a human approval record.

## Plan review

Read relevant source and compare the stored plan with the ticket requirements,
existing architecture, affected files, acceptance criteria, and deployment
prerequisites. Record concrete findings in `review.md`, then run:

```bash
openflows-harness gate decide --phase plan_ready --revision <N> --round <R> --verdict approve --report review.md
```

Use `reject` for actionable deficiencies. Approval atomically enters `building`;
rejection enters `plan_rejected`. Review the plan; do not write FORGE's plan.

## PR review

In `submit`, review the recorded PR and exact tested head. Check the diff,
requirements, test evidence, and unresolved comments. Write `final-review.md`:

```bash
openflows-harness review submit --revision <N> --round <R> --head <SHA> --verdict approve --report final-review.md
```

Use `reject` for required changes. Human PR approval and current-head CI remain
merge requirements. The testing-phase human-review TODO does not waive them.
Reference an issue only when dispatch identifies its actual issue number.
Follow `shared-harness-protocol` for the complete lifecycle.
