---
name: forge-ci-fix
description: Handle a VESSEL-dispatched /ci_fix directive — reuse your existing workspace and branch to fix failing CI checks and re-arm the PR
---

# FORGE CI Fix Skill

You are FORGE, and VESSEL has dispatched a `/ci_fix` directive into your **existing
chat session** because CI checks failed on a PR you authored. You fix the failing
checks in the **same workspace and branch** — you do not provision a new workspace
or start the work from scratch.

## When to Use

- You receive a `/ci_fix` chat message from VESSEL.
- It carries `state: ci_failed`, the `pr` number, the `ticket` id, the `branch`,
  and the failing check / annotation / job-log details.

## Steps

1. **Read the directive** — note the PR number, failing check names, and any
   `path:line` annotations. You are already on the PR branch.

2. **Confirm you are on the right branch** and have the latest base merged in:
   ```bash
   git fetch origin <default> && git merge origin/<default>
   ```
   Resolve any conflict markers by integrating both sides — never just pick one.

3. **Match checks to workflows** — read `.github/workflows/` and map each failed
   check name to its job in the workflow YAML.

4. **Reproduce and fix locally** — install the tools/deps the workflow expects, run
   the failing job's exact `run:` steps, and fix **all** errors (not just the first).

5. **Verify all checks pass locally**, then push:
   ```bash
   git add -A && git commit -m "fix CI failures" && git push
   ```
   Never force-push and never bypass branch protection.

6. **Re-arm the PR for review**:
   ```bash
   openflows-harness status set review_ready
   ```
   VESSEL re-polls CI and SENTINEL re-reviews the updated head. Stay in the same
   chat session.

## Rules

- Stay in the **same chat session / workspace** — reuse your existing workspace and
  branch.
- Never force-push or bypass branch protection.
- Do not change the PR title or description unless asked.
- Fix all failing checks before re-arming.

## Blocked If

- You cannot reproduce or resolve the failure — set `status set blocked` with an
  exact, answerable question rather than force-pushing.

See `orchestration/plugin/commands/ci_fix.md` for the full command contract.
