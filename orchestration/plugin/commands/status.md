---
name: status
description: Read or advance the authoritative ticket lifecycle
---

# /status

Read `openflows-harness status get` before acting. It returns the phase, version,
plan revision, review round, candidate head, decisions, feedback and history.

| Phase | Entry / exit |
|---|---|
| planning | Write the plan at the current chat-specific path; upload with `plan write --file <absolute-plan-path>` |
| plan_ready | Submit the uploaded plan; wait for SENTINEL approval |
| plan_rejected | Read feedback, set planning, revise/upload and resubmit |
| building | Implement the approved plan, commit changes |
| testing | Set with a clean checkout; run verify serve, await SENTINEL + human approval |
| submit | Set after testing approval; open/record PR, await SENTINEL + human approval + CI |
| done | Controller-only, confirmed merge; terminal |
| blocked | Record blocker; recover through planning |

Use `openflows-harness status set <phase>` for permitted worker transitions.
A draft plan is not permission to edit source. Testing and submit freeze source;
return to building for fixes, then repeat testing and review. A plan change
requires returning to planning and getting a new approval. Skipped stages and
stale decisions are rejected. `review_ready` is no longer a phase.
