---
name: review
description: Record a SENTINEL decision for a specific lifecycle review round
---

# /review

In testing, read `.agents/skills/sentinel-testing/SKILL.md` before selecting
checks. Derive and document the strategy from the exact approved plan DoD,
candidate diff and repository conventions. Reconsider irrelevant commands before
requesting installation; missing dependencies for necessary checks require
FORGE repair through rejection into building. Report inspection-only evidence
limitations and unresolved criteria; never fabricate success or bypass A2A.

Read `openflows-harness status get` and `plan read`. Use the returned `revision`,
`review_round` and `head` from the artifact you actually review, not from a later
snapshot obtained merely to make an outdated command succeed. Write a report.

For a submitted plan:
```bash
openflows-harness gate decide --phase plan_ready --revision <N> --round <R> --verdict approve --report review.md
```
For testing, replace the program/argument placeholders with meaningful checks
selected from the DoD strategy. Verify implementation against the approved plan through
A2A in FORGE's clean checkout (FORGE runs `verify serve`):
```bash
openflows-harness verify request --expect-exit 0 -- <program> <arguments>
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
