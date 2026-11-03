---
name: forge-coding
description: Use when FORGE implements an approved ticket or addresses review and CI feedback.
---

# FORGE Coding Skill

## Before implementing

Read `openflows-harness status get`, `openflows-harness dispatch read`, and
`openflows-harness plan read`. Read applicable repository standards and the
current review feedback. Read-only source inspection is allowed during planning; source
edits require the current `building` phase. SENTINEL plan approval atomically
enters `building`; FORGE does not activate it with a separate status command.
A queued approval message cannot override a newer `blocked` phase.

Use `git branch --show-current` and `git status --short` to confirm the actual
assigned checkout. Keep that branch through implementation, rework, and PR
creation. If the checkout is main or another protected base, or disagrees with
the assignment, report the mismatch before source edits. Dispatch may identify
a base branch; do not assume that means the working branch should be main.
Do not construct a branch name from legacy environment variables or infer issue numbers from
ticket IDs.

## Coding and verification

- Follow repository coding standards, architecture patterns, and API contracts
  when present; inspect similar implementations before inventing new ones.
- Implement the approved plan in focused segments and verify each meaningful
  behavior change with appropriate tests. Use the repository's actual test and
  lint commands; do not assume a particular tooling script exists.
- Follow project conventions for errors, asynchronous operations, timeouts, and
  retries. Do not add broad refactors unrelated to the ticket or review.
- Keep track of changed files, verification results, and unresolved limitations.
  Segment boundaries are implementation checkpoints, not separate review gates.
- Honor file ownership and policy denials. Record a specific blocker through the
  harness if needed; do not delegate or disguise operations to bypass policy.

## Testing and PR gates

1. Complete local verification, commit the implementation, and leave the checkout clean.
2. Run `openflows-harness status set testing`, then serve verification with
   `openflows-harness verify serve`. Source remains frozen during testing.
3. Wait for successful A2A verification and SENTINEL testing approval for the
   current revision, round, and commit. Human testing review is TODO, not a blocker. Read harness status on notification; do not poll in a loop.
4. After SENTINEL approves testing, read `openflows-harness status get` and
   proceed only when the lifecycle is `submit`. The approval moves the shared
   lifecycle to `submit`; do not repeat the transition. Push the assigned branch
   with `git push` using the workspace's brokered Git credential.
5. If no PR number is recorded, run `openflows-harness pr create --repo OWNER/REPO
   --base BASE --head <actual-branch> --title "Descriptive title" --body "Markdown description"`.
   Use the task's target base branch, or the repository default if none is
   specified; do not assume `main`. The title and body must explain the actual
   change, why it was needed, verification results, and any required public
   deployment link. Reference an issue only if dispatch identifies it. This
   single command finds or creates the GitHub PR and records it in SharedStore;
   no `gh`, GitHub MCP, or separate `pr opened` step is required. If the PR is
   already recorded, reuse it and push updates to its branch; `pr create` does
   not edit an existing PR's title or body.
6. Use `pr opened --pr <N> --branch <actual-branch> --title <title>` only to
   recover by recording a PR that was created externally, while in `submit`.
   Wait for SENTINEL and human PR approval. VESSEL handles merge readiness;
   missing or timed-out CI is not success.

Do not force-push or bypass branch protection. If pushing, deployment, or PR
creation fails, report the precise error and required capability. A deployment
task is not complete merely because configuration was committed: verify the
requested running service and public link, or record the unmet prerequisite.

## Review, conflicts, and CI rework

Read every actionable finding and inline comment. For `/address_review` or
`/ci_fix`, reuse the existing workspace and PR branch. Read current harness
state and enter `building` through the permitted rework transition before
editing frozen source. If that transition is denied, report it and stop.

Resolve conflicts by integrating both sides deliberately. Match failed checks
to the repository workflows, reproduce failures, and fix all relevant errors.
Address feedback without unrelated refactors. Run the relevant verification,
commit, push, and repeat `testing`, both testing approvals, then `submit` and
both PR approvals for the new head. Never jump directly from rework to submit.

## Blockers and continuity

When SENTINEL returns a failing command or executor setup diagnostic, fix it in
building under the existing approved plan, then return to testing. Use the same
project tool setup for builds and verification; persist activation in the user's
login profile or invoke a project script. Checkout-local dependencies must be
installed in the temporary checkout with the project's normal commands.
Use `openflows-harness status set blocked` only for an external prerequisite you
cannot resolve, with one precise unblock question and evidence. Its recovery
returns to planning. Do not retry unchanged configuration/policy failures in a loop.

Before context reset, preserve progress and remaining work using
`openflows-harness handoff write --contract <file> --notes <notes>`. On resume,
read harness state before acting. Follow `shared-harness-protocol` for lifecycle
and coordination; local status files do not drive the controller.
