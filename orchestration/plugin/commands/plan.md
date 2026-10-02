# /plan

Read `.agents/skills/forge-planning/SKILL.md` and
`.agents/skills/shared-harness-protocol/SKILL.md` before planning. Command

Create a grounded implementation plan for the current ticket.

## Gather evidence first

1. Read `openflows-harness status get` and `openflows-harness dispatch read`.
   Read `openflows-harness plan read` when revising an existing plan.
2. Inspect relevant source files, tests, repository structure, and architecture
   guidance read-only before writing the plan. This inspection is allowed before
   approval; source changes remain gated.
3. Check the actual checkout and branch. For deployment tasks, inspect existing
   deployment configuration and runtime capabilities without starting containers,
   provisioning resources, or exposing secrets. Identify the service, target,
   and public exposure mechanism. Report precise missing prerequisites.

## Plan structure

- **Understanding:** requested outcome and acceptance criteria.
- **Repository findings:** observed behavior and relevant file paths.
- **Approach:** concrete changes using existing patterns.
- **Segments:** deliverable, files, verification, and measurable exit condition.
- **Risks and prerequisites:** evidence, assumptions, and focused questions.
- **Out of scope:** unrelated changes excluded from the implementation.

Write the plan at the exact chat-specific path supplied by Coder or the startup hook,
`/home/coder/.coder/plans/PLAN-<chat-id>.md`. Do not use `/home/coder/PLAN.md`.
Replace `<absolute-plan-path>` below with that exact path. Size segments for independent verification;
there is no separate segment-review waiting protocol.

## Submit

```bash
openflows-harness plan write --file <absolute-plan-path>
openflows-harness status set plan_ready
```

The harness stores the plan for SENTINEL to review by revision and round. Do not
stage unrelated changes or commit to a protected base branch to submit a plan.
Halt and wait for the review notification without polling in a loop.

SENTINEL approval atomically transitions shared state to `building`. Read
`openflows-harness status get` on notification and start implementation only if
the current phase is still `building`; no separate FORGE transition is needed.
For rejection, read harness feedback, return to planning, revise, and resubmit.
Follow `forge-planning` and `shared-harness-protocol` for the full lifecycle.
