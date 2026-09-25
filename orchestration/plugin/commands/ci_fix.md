---
name: ci_fix
description: Address a VESSEL-dispatched CI failure directive (failed checks / annotations / job logs) and re-arm the PR for review
---

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
   openflows-harness status set review_ready
   ```
   This tells the controller the PR is ready again. VESSEL re-polls CI, and
   SENTINEL re-reviews the updated head.

## Rules

- Stay in the **same chat session / workspace** — reuse your existing workspace and
  branch. Do not provision a new one.
- Never force-push or bypass branch protection.
- Do not change the PR title or description unless asked.
- Fix all failing checks before re-arming — do not write `review_ready` until your
  local run matches the workflow's expected checks.

## Blocked If

- You cannot reproduce or resolve the failure — leave the PR in its current state
  and set `status set blocked` with an exact, answerable question rather than
  force-pushing.
