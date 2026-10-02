---
name: ci_fix
description: Address a VESSEL-dispatched CI failure directive (failed checks / annotations / job logs) and re-arm the PR for review
---

## Authoritative lifecycle contract

Read `openflows-harness status get` before acting. The lifecycle is
`planning -> plan_ready -> building -> testing -> submit -> done`.
Start/revise in `planning`; upload with `plan write --file <absolute-plan-path>`, then set
`plan_ready`. SENTINEL reviews the exact `revision` and `review_round`. A rejection
enters `plan_rejected`; FORGE returns to `planning`, revises, and resubmits.
No source edits are allowed before approval.

After building, commit all changes, then set `testing`. Keep the checkout clean
and run `openflows-harness verify serve` in FORGE. SENTINEL runs tests through
`verify request --expect-exit 0 -- <program> <arguments>`, writes a report, and
uses `gate decide --phase testing --revision <N> --round <R> --head <SHA>
--verdict approve --report review.md` (or reject). Testing requires successful A2A verification and SENTINEL approval.
TODO(human-testing-review): add human approval later; it does not block submit now. Then FORGE sets `submit`, opens/updates and records the PR.
SENTINEL records the PR verdict with `review submit --revision <N> --round <R>
--head <SHA> --verdict approve --report final-review.md` (or reject). Read the
current round again after recording a PR. Humans use the operator CLI
`openflows gate decide --tenant <tenant> --ticket <ticket> --phase testing|submit
--revision <N> --round <R> --head <SHA> --verdict approve|reject --notes <reason>`.

Every rework cycle returns to `building`, then repeats testing and both review
gates. Never jump directly from building to submit. Testing/submit freeze source.
VESSEL requires current-head CI success, SENTINEL and human PR approval, and
confirmed merge before done. Missing or timed-out CI never counts as success.
Use `blocked` for an operational failure; recovery returns to planning.



# /ci_fix Command

VESSEL dispatches `/ci_fix` into your **existing chat session** when CI checks
failed on a pull request you authored. You address the failing checks in the
**same workspace and branch** you already have checked out — you do **not** start
work from scratch.

## When to Use

Received from VESSEL (as a chat message) when a PR you opened has a failing CI
check:

- `state: ci_failed` — one or more checks/jobs failed on the PR head commit
- The directive carries the PR number, the failure reason, and (when available)
  the failed-check names, `path:line` annotations, and job log output.

## Directive Format

VESSEL sends a structured directive that may look like:

```
/ci_fix
state: ci_failed
pr: 42
ticket: T-034
branch: forge-1/T-034
reason: <short failure summary>
checks:
- <check/job name>
annotations:
- **<check>** `src/api.rs:78` <message>
```

## Steps

1. **Read the directive.** Note the PR number, the failing check names, and any
   `path:line` annotations. You are already on the PR branch — do not create a
   new branch or workspace.

2. **Confirm you are on the right branch** and have the latest base merged in:
   ```bash
   git fetch origin <default> && git merge origin/<default>
   ```
   Resolve any conflict markers (`<<<<<<<`, `=======`, `>>>>>>>`) by integrating
   both sides — never just pick one.

3. **Match checks to workflows.** Read `.github/workflows/` and match each failed
   check name to its job in the workflow YAML.

4. **Reproduce and fix locally.**
   - Install any tools the workflow expects (pip, npm, ruff, etc.).
   - Install project deps as the workflow does.
   - Run the failing job's exact `run:` steps from the workflow YAML.
   - Fix **ALL** errors — do not stop at the first; CI will just fail on the next.

5. **Verify all checks pass locally**, then push:
   ```bash
   git add -A && git commit -m "fix CI failures" && git push
   ```
   **Never force-push** and **never bypass branch protection.**

6. **Re-arm the PR for review**
   ```bash
   openflows-harness status set testing
   ```
   This tells the controller the PR is ready again. VESSEL re-polls CI, and
   SENTINEL re-reviews the updated head.

## Rules

- Stay in the **same chat session / workspace** — reuse your existing workspace and
  branch. Do not provision a new one.
- Never force-push or bypass branch protection.
- Do not change the PR title or description unless asked.
- Fix all failing checks before re-arming — do not write `submit` until your
  local run matches the workflow's expected checks.

## Blocked If

- You cannot reproduce or resolve the failure — leave the PR in its current state
  and set `status set blocked` with an exact, answerable question rather than
  force-pushing.
