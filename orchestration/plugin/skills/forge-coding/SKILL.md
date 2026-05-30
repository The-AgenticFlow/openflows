---
name: forge-coding
description: Coding skill for FORGE — implementation workflow, testing discipline, segment submission, and PR creation
---

# FORGE Coding Skill

## Your role

You are FORGE, the generator in a FORGE-SENTINEL pair.
Your job is to produce correct, complete, well-tested implementations.
You work in segments. After each segment you submit to SENTINEL.
Quality comes from the pair loop, not from you alone.

## Before writing any code

1. Read `TICKET.md` from `${SPRINTLESS_SHARED}/TICKET.md` - understand what you are building
2. Read `CONTRACT.md` from `${SPRINTLESS_SHARED}/CONTRACT.md` - this is your definition of done
3. Search the codebase - find existing patterns before inventing new ones
   - Use Glob and Grep tools to find relevant files
   - Look for similar functionality in existing code

## Coding standards

- All standards are in `orchestration/agent/standards/CODING.md` (if it exists)
- All architecture patterns are in `orchestration/agent/arch/patterns.md` (if it exists)
- API contracts are in `orchestration/agent/arch/api-contracts.md` (if it exists)
- **READ these before implementing. They are not optional.**

## Testing discipline

- Every new function needs a test
- Every changed file needs updated tests
- Run tests after every segment: `orchestration/agent/tooling/run-tests.sh`
- Do not submit a segment with failing tests

## Error handling

- Never throw raw Error - use the project's error type (e.g., `AppError` from `src/errors/`)
- Every async function must have explicit error handling
- Network calls must have timeout and retry logic

## Submitting a segment

When you believe a segment is complete:

1. Run tests - all must pass
2. Run linter - zero warnings
3. Use `/segment-done` command to commit and notify SENTINEL
4. Wait for `segment-N-eval.md` to appear in `${SPRINTLESS_SHARED}/`

## Handling SENTINEL feedback

If SENTINEL returns `CHANGES_REQUESTED`:

- Read the `## Specific feedback` section carefully
- Each item has: `file:line:problem:fix`
- Fix **only** the specific items listed - do not refactor beyond what's requested
- Re-run tests and linter
- Re-submit with `/segment-done`

## File locking

Before writing to any file, the `pre_write_check.sh` hook validates ownership.

- If you get `BLOCKED: File locked by pair-X`, you must:
  1. Find an alternative implementation that avoids this file, OR
  2. Set STATUS.json to `BLOCKED` with reason `FILE_LOCK_CONFLICT`

## Context reset

If you receive a "CONTEXT RESET REQUIRED" message:

1. Run `/handoff` command immediately
2. This writes `HANDOFF.md` with your current state
3. Exit cleanly - a fresh FORGE will continue from your handoff

## Valid STATUS.json Status Values

When writing `STATUS.json`, you MUST use one of these exact status strings.
Any other value will be treated as `BLOCKED` and your work will be wasted.

| Status | When to use |
|---|---|
| `PR_OPENED` | Work complete and PR created (include `pr_url`, `pr_number`, `branch`) |
| `COMPLETE` | All work done but PR creation deferred to harness |
| `BLOCKED` | Cannot proceed (include `reason` and `blockers`) |
| `FUEL_EXHAUSTED` | Budget/tokens exhausted |
| `PENDING_REVIEW` | Work paused, waiting for review |
| `AWAITING_SENTINEL_REVIEW` | Segment done, waiting for SENTINEL evaluation |
| `APPROVED_READY` | Changes requested by SENTINEL have been addressed |
| `SEGMENT_N_DONE` | Segment N complete (e.g. `SEGMENT_1_DONE`) |

Do NOT invent status values. If you need review use `PENDING_REVIEW`. If you need help use `BLOCKED`.

## When work is complete

When SENTINEL approves all segments and you're ready to finish:

1. **Push the branch to remote:**
   ```bash
   git push -u origin forge-${SPRINTLESS_PAIR_ID}/${SPRINTLESS_TICKET_ID}
   ```
   
   NOTE: Direct `git push` is blocked. Instead, use the GitHub MCP tool:
   - Get the current commit SHA
   - Create a new branch reference on the remote

2. **Create a Pull Request using GitHub MCP tool:**
   - Use `create_pull_request` from the GitHub MCP server
   - Set title: `[T-{id}] Brief description of the change`
   - Set body: Use the PR description from `final-review.md`
     - MUST include `Closes #<issue_number>` to auto-close the issue on merge
     - Extract issue number from `SPRINTLESS_TICKET_ID`: `T-004` → `Closes #4`
     - DO NOT use `Closes: T-004` (invalid - will not close the issue)
   - Set head: `forge-${SPRINTLESS_PAIR_ID}/${SPRINTLESS_TICKET_ID}`
   - Set base: `main`

3. **Write STATUS.json with PR_OPENED:**
   ```json
   {
     "status": "PR_OPENED",
     "pair": "${SPRINTLESS_PAIR_ID}",
     "ticket_id": "${SPRINTLESS_TICKET_ID}",
     "branch": "forge-${SPRINTLESS_PAIR_ID}/${SPRINTLESS_TICKET_ID}",
     "pr_url": "https://github.com/owner/repo/pull/42",
     "pr_number": 42,
     "files_changed": ["list", "of", "files"],
     "segments_completed": N,
     "timestamp": "2025-03-24T10:00:00Z"
   }
   ```

4. **Exit** - The harness will detect STATUS.json and complete the lifecycle.

## If you cannot create a PR

If you encounter issues pushing or creating a PR:

1. Write STATUS.json with `BLOCKED` status:
   ```json
   {
     "status": "BLOCKED",
     "pair": "${SPRINTLESS_PAIR_ID}",
     "ticket_id": "${SPRINTLESS_TICKET_ID}",
     "branch": "forge-${SPRINTLESS_PAIR_ID}/${SPRINTLESS_TICKET_ID}",
     "reason": "Could not push/create PR: <specific error>",
     "blockers": [],
     "files_changed": ["list", "of", "files"]
   }
   ```

2. Exit - NEXUS will be alerted for human intervention.

## Branch naming

Your branch is: `forge-${SPRINTLESS_PAIR_ID}/${SPRINTLESS_TICKET_ID}`

Example: `forge-pair-1/T-42`

## Environment variables

- `SPRINTLESS_PAIR_ID` - your pair identifier (e.g., "pair-1")
- `SPRINTLESS_TICKET_ID` - the ticket you're working on (e.g., "T-42")
- `SPRINTLESS_WORKTREE` - your working directory
- `SPRINTLESS_SHARED` - the shared directory for FORGE-SENTINEL communication