---
name: forge-planning
description: Use when FORGE prepares or revises a ticket implementation plan before SENTINEL approval.
---

# FORGE Planning Skill

## Ground the plan before writing it

Read `openflows-harness status get` and `openflows-harness dispatch read` for the
current phase, assignment, and requirements. On resume, also read
`openflows-harness plan read`. Harness state is authoritative.

Before writing the plan, inspect relevant source files, repository structure,
existing tests, architecture guidance, and deployment configuration. Read-only
source inspection is allowed before plan approval. Use targeted searches and
reads to identify the actual implementation and existing patterns; do not write
a speculative plan that merely promises to inspect the repository later.

Check `git status --short` and `git branch --show-current`. Use the assigned
checkout and branch. If it is the protected base branch or conflicts with the
assignment, report the mismatch before implementation; do not edit source on
main or invent another branch from legacy environment variables. A base branch
in dispatch is not evidence that main is the assigned working branch.

For deployment tickets, inspect existing compose files, Dockerfiles, deployment
documentation, and available runtime capabilities read-only. Identify the
service, deployment target, container runtime, and public exposure mechanism.
If an essential prerequisite is unavailable or unspecified, report exactly
what is missing and the evidence. Do not invent infrastructure, provision
resources during planning, or keep exploring unrelated alternatives. Avoid
printing credentials, environment dumps, or rendered configuration secrets.

Planning permits the standard current-chat plan and coordination artifacts. Source edits, dependency
installation, builds, container startup, and deployment wait for approval.
If policy denies a read-only probe, record the exact command and denial for
NEXUS; do not delegate or disguise the operation to bypass the hook.

## Plan structure

1. **Understanding:** requested outcome and acceptance criteria from dispatch.
2. **Repository findings:** relevant source/configuration paths and what already exists.
3. **Technical approach:** concrete changes following existing architecture.
4. **Segments:** focused, testable steps with files and measurable exit conditions.
5. **Verification:** commands and evidence needed to establish the requested outcome.
6. **Risks and prerequisites:** known limitations, missing capabilities, and precise questions.
7. **Out of scope:** boundaries that prevent unrelated changes.

Keep segment sizes proportionate to the task. Reuse existing deployment and
coding patterns. Distinguish observed facts from assumptions.

## Plan file path

Use the exact path supplied by Coder or the startup hook:
`/home/coder/.coder/plans/PLAN-<chat-id>.md`. The ID belongs to the current chat;
do not invent it, reuse another chat's file, or write `/home/coder/PLAN.md`.
Legacy `PLAN.md` and `plan` aliases are not accepted for planning or upload.
Replace `<absolute-plan-path>` in commands with that same absolute path.
The harness accepts this file directly; no copy into the repository is needed.

## Submit and resume

```bash
openflows-harness plan write --file <absolute-plan-path>
openflows-harness status set plan_ready
```

Upload through the harness so SENTINEL reads the exact stored revision. Wait
for its decision; do not poll in a loop or start implementation while waiting.

SENTINEL approval atomically transitions shared state to `building`. FORGE
must not perform a separate transition to building to activate an approval.
On notification, read `openflows-harness status get` and start only if the
current phase is `building`. An old approval notification does not override
`blocked` or another newer phase.

If rejected, read the harness review feedback, return from `plan_rejected` to
`planning`, address the findings in the current-chat plan, upload, and set `plan_ready` again.
Use the current revision and review round; do not wait for local contract or
per-segment review files. Follow `shared-harness-protocol` for later gates.
