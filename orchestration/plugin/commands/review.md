---
name: review
description: Record a SENTINEL decision for a specific lifecycle review round
---

# /review

Send project verification commands to FORGE with `verify request`. Each runs in a
temporary checkout using FORGE's current login-shell environment. The response
includes argv, candidate HEAD, exit code, stdout and stderr. Correct malformed
arguments first. For failing commands, timeouts, missing dependencies or executor
setup failures, record those details in `review.md` and reject the current testing
round with actionable feedback. This returns FORGE to building under the existing
approved plan. Distinguish a failed test from a command that could not start.
Transport errors mean evidence is missing, not that tests failed; report the exact
error and return actionable executor repair to FORGE. Reserve blocked for external
prerequisites FORGE cannot resolve. Never approve smoke tests as acceptance evidence.


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
