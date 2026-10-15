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

Read `.agents/skills/sentinel-testing/SKILL.md` and follow it before selecting
commands. Derive and document sufficient verification from the exact approved
plan's DoD, candidate diff, ticket criteria, and repository conventions. Map each
DoD item to inspection or meaningful checks, observed evidence, and unresolved
gaps in `review.md`. Explain why selected checks suffice and omitted builds are
unnecessary. There is no universal Cargo/build command or file-type exemption.

FORGE commits the candidate, enters testing with a clean checkout, and runs
`openflows-harness verify serve`. Send the selected executable checks through
`openflows-harness verify request --expect-exit 0 -- <program> <arguments>`.
Pass separate executable/argument tokens after `--`; options such as
`--timeout-secs` go before it. Do not use a single quoted `--argv` command.
Inspect task ID, argv, exit, output, timeout, and candidate HEAD. Local checks
cannot replace A2A evidence, and a successful probe cannot prove acceptance.

Reconsider irrelevant commands before requesting tool installation. Necessary
check dependencies that FORGE can repair require testing rejection into building
with precise diagnostics and the DoD gap; external prerequisites alone warrant
blocked. Do not claim tests ran when they could not start. Transport failures
mean missing evidence. If inspection alone is meaningful, document the current
executable-evidence limitation and request clarification or authorized plan
revision; do not fabricate a successful task, weaken the DoD or bypass the gate.

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
prerequisites. Surface ambiguous DoD items and inspection-only criteria that the
current executable A2A gate cannot represent; do not invent meaningless checks.
Record concrete findings in `review.md`, then run:

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
