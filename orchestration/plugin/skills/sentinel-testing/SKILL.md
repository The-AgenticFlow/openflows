---
name: sentinel-testing
description: Use in testing to derive sufficient candidate-bound A2A verification from the exact approved plan Definition of Done.
---

# SENTINEL Testing Strategy

SENTINEL owns evidence selection. The approved plan's Definition of Done (DoD)
is the primary contract, not a language, file extension, or universal test command.
Do not route verification by file type or exempt documentation from verification.

## Establish the contract before selecting commands

1. Read `openflows-harness status get`, `dispatch read`, and `plan read`.
   Record the exact approved revision, testing review round, and candidate HEAD
   in `review.md`. Read that revision's DoD and the ticket acceptance criteria.
2. Inspect the candidate implementation and diff, repository instructions,
   conventions, verification scripts, and relevant dependencies. Check that the
   plan and implementation address the ticket. Do not replace the approved plan
   with a local CONTRACT.md or remembered plan from an earlier round.
3. Before requesting execution, write a verification strategy in `review.md`.
   For every DoD item, identify the observable evidence that would establish it:
   direct inspection, static assertions, behavioral tests, integration checks,
   or builds. Include implementation-driven regression risks beyond the DoD.
4. Select the smallest sufficient set of checks. Explain each check's criterion
   or regression risk, expected result, and necessary dependencies. Explain why
   omitted broader tests/builds add no necessary evidence. Broaden checks when
   warranted; neither a narrow nor a full test suite automatically proves the DoD.

Documentation can require content consistency checks or compiled examples and
runtime tests. The same file may need different evidence under different DoDs.
Rust changes may require targeted tests or broader integration/build checks.
Other repositories use their own tools and conventions. These are reasoning
examples, not command-selection rules. Do not install Rust merely because
OpenFlows itself is written in Rust.

## Execute meaningful checks through A2A

FORGE commits all changes, keeps the candidate checkout clean, and runs
`openflows-harness verify serve`. Request each selected check using:

```text
openflows-harness verify request --expect-exit 0 -- <program> <arguments>
```

Replace the placeholders with the selected project command. Pass the executable
and arguments as separate tokens after `--`; never send a whole command as one
`--argv` token. Harness options such as `--timeout-secs` go before `--`.
Shell operators and expansions after `--` are literal arguments. Correct
tokenization/argument mistakes and retry the justified check.

Each command runs in a temporary checkout at FORGE's candidate HEAD using its
current login-shell environment. Inspect task ID, argv, tested HEAD, expected
and actual exit, timeout, stdout, stderr, and any requested artifacts. Local
SENTINEL tests and earlier-head results cannot substitute for A2A evidence.
Meaningful content assertions must fail when the claimed content property is
false; printing a file, listing files, `echo`, `true`, or a transport smoke probe
does not establish acceptance merely because it exits zero.

## Interpret failures without creating irrelevant repair loops

- **Command-selection mistake:** Reconsider whether the command establishes a
  required criterion/risk before requesting tools. Record the failed attempt and
  why it is irrelevant; select and execute sufficient replacement checks in this
  testing round. Missing tools for an irrelevant command do not justify installing
  them. Do not relabel a necessary failing check as irrelevant to obtain approval.
- **Necessary setup failure:** If a justified check cannot start or lacks a
  dependency, record the diagnostics and the DoD item it leaves unverified.
  Reject the current testing round into `building` with actionable FORGE tool or
  environment repair. Do not claim a test ran or succeeded. SENTINEL installs no
  tools. Reserve `blocked` for external prerequisites FORGE cannot resolve, such
  as unavailable credentials or an external service; not repairable workspace tools.
- **Unmet criterion or regression:** Reject with concrete source/test feedback.
- **Transport failure, timeout, absent or stale evidence:** Report exactly what
  is missing. Do not infer test failure or success from a relay failure. Return
  actionable executor repair through building when appropriate; avoid retry loops.
- **Insufficient evidence:** A passing command for one criterion does not prove
  the rest. Obtain the missing evidence, or reject with explicit gaps.

## Inspection-only limitation and decisions

The current lifecycle requires successful executable A2A verification even when
inspection establishes the entire DoD. There is no inspection-only approval mode.
Use a lightweight executable assertion only if it establishes a real DoD property
or relevant regression risk. If no meaningful executable check exists, document
the evidence-model limitation and reject with a request for clarification or an
authorized plan revision. Do not add an arbitrary criterion, fabricate a task,
run a meaningless command, weaken the DoD, or bypass the gate. During plan review,
surface this constraint early. Rejection returns to building under the approved
plan; it does not authorize source changes outside that plan or silently restart
planning. If clarification requires a changed DoD, FORGE can explicitly use the
existing `status set planning` transition, upload a revised plan, set `plan_ready`,
and obtain new approval before building. Do not use replanning for a repairable
tool failure or silently replace an approved acceptance criterion.

Complete `review.md` with:

| DoD item / risk | Inspection or selected check and rationale | Observed evidence (task, argv, HEAD, output/artifact, exit) | Result / unresolved gap |
|---|---|---|---|

Keep the initial strategy, including changes in selection and omitted checks,
and record actionable file/line fixes. Approve only when every DoD item and
necessary regression check is established and successful A2A evidence exists for
the candidate. Re-read status to detect changes; a changed revision, round, or
HEAD requires a fresh review, not relabeling old evidence. Record:

```text
openflows-harness gate decide --phase testing --revision <N> --round <R> --head <SHA> --verdict approve --report review.md
```

Use `reject` for unresolved gaps. Rework repeats testing for the new candidate.
Human testing review remains TODO; human PR approval and current-head CI remain
required in submit. Follow `sentinel-review` and `shared-harness-protocol` for
the other review phases. A2A success records execution, not semantic sufficiency;
SENTINEL must assess the evidence rather than equating exit zero with completion.
