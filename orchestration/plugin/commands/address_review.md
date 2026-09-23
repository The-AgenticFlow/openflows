---
name: address_review
description: Address a VESSEL-dispatched review directive (conflicts / changes_requested / comments) and re-arm the PR for review
---

# /address_review Command

VESSEL dispatches `/address_review` into your **existing chat session** when your PR
is in a GitHub-native review state that blocks merging: **conflicts**, **changes
requested**, or **unaddressed review comments**. Address the feedback, push, and
re-arm the PR for review.

## When to Use

Received from VESSEL (as a chat message) when the PR is not merge-ready because:

- `state: conflicts` — the PR has merge conflicts with the base branch
- `state: changes_requested` — a reviewer (e.g. SENTINEL) requested changes on GitHub
- `state: comments` — inline review comments are present

## Directive Format

VESSEL sends a structured directive that may look like:

```
/address_review
state: changes_requested
pr: 42
reason: <latest review body>
comments:
- src/api.rs:78  missing pagination
Conflicts: <files...>   (when state: conflicts)

Then re-run: openflows-harness status set review_ready
```

## Steps

1. **Read the directive.** Note the `state`, the inline `comments` (`path:line` +
   message), and any conflicted files.

2. **Resolve conflicts** (when `state: conflicts`)
   - Fetch the latest base branch: `git fetch origin <default>` and merge it in.
   - Resolve every conflict marker (`<<<<<<<`, `=======`, `>>>>>>>`) — integrate
     both sides; do **not** just pick one.
   - Stage and commit the resolutions.

3. **Address each inline comment / review point**
   - Open each `path:line` and apply the requested change.
   - Fix **all** comments, not just the first one — the reviewer will re-check.
   - For `changes_requested`, address the review body's guidance directly.

4. **Verify** your work compiles and, where applicable, local checks pass.

5. **Push** your changes to the PR branch. **Never force-push** and **never bypass**
   branch protection.

6. **Re-arm the PR for review**
   ```bash
   openflows-harness status set review_ready
   ```
   This tells the controller that the PR is ready for another review pass. VESSEL
   will re-poll, and SENTINEL will re-review on the updated head.

## Rules

- Stay in the **same chat session** — do not start a new workspace or session.
- Never force-push or bypass branch protection.
- Do not change the PR title or description unless asked.
- If you resolve conflicts or change the head, push so GitHub re-evaluates
  `mergeable` and re-runs CI before you re-arm.

## Blocked If

- You are unable to resolve the conflicts or comments — leave the PR in the rework
  state and report the blocker rather than force-pushing.
