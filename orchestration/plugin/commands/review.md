---
name: review
description: Record a SENTINEL decision for a specific lifecycle review round
---

# /review

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
or sandbox setup blockers and stop.
Project-specific verification commands are allowed in the mandatory sandbox. Known
destructive/control-plane operations have explicit denials. `echo hello` checks
transport only and never proves acceptance criteria. Missing toolchains or offline
dependencies require the operator to update the verification image.


Read `openflows-harness status get` and `plan read`. Use the returned `revision`,
`review_round` and `head` from the artifact you actually review, not from a later
snapshot obtained merely to make an outdated command succeed. Write a report.

For a submitted plan:
```bash
openflows-harness gate decide --phase plan_ready --revision <N> --round <R> --verdict approve --report review.md
```
For testing, verify implementation against the approved plan and run tests through
A2A in FORGE's clean checkout (FORGE runs `verify serve`):
```bash
openflows-harness verify request --expect-exit 0 -- cargo test --workspace --all-features
openflows-harness gate decide --phase testing --revision <N> --round <R> --head <SHA> --verdict approve --report review.md
```
For a recorded PR in submit:
```bash
openflows-harness review submit --revision <N> --round <R> --head <SHA> --verdict approve --report final-review.md
```
Use `--verdict reject` with actionable feedback for any failed review. Rejected
plans return to plan_rejected; testing/PR rejection returns to building. The
controller retries delivery of PR decisions to GitHub from a persisted queue.
Do not substitute a GitHub-only review for the lifecycle command.

Read `.agents/skills/sentinel-review/SKILL.md` and
`.agents/skills/shared-harness-protocol/SKILL.md` before reviewing.
Human testing review is TODO; A2A verification and SENTINEL approval gate testing.
Human PR approval remains required in submit; CI must succeed for the submitted head.
