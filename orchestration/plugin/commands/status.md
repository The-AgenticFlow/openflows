---
name: status
description: Signal the current harness phase for the ticket
---

# /status Command

Signal your current progress phase to the harness. Run via the CLI:

```bash
openflows-harness status set <phase>
```

## Authoritative Phases

| Phase | When to use |
|---|---|
| `planning` | Analyzing the ticket and writing `PLAN.md`; wait for SENTINEL gate approval |
| `building` | Implementing after SENTINEL approves the plan |
| `testing` | Running the test suite and verifying behavior |
| `review_ready` | PR is open and SENTINEL is reviewing the completed work |
| `blocked` | Cannot proceed — include an exact, answerable question |

Do NOT invent other phase values (e.g. `AWAITING_REVIEW`, `COMPLETE`, `PR_OPENED`,
`PENDING_REVIEW`). The harness rejects unknown phases.

## What it does

Writes `ticket:{id}:status` (`{"phase","role","ts"}`) to SharedStore. The controller
(NEXUS) reads this key to route the ticket — it does **not** read a `STATUS.json` file.

## Examples

### Enter planning

```bash
openflows-harness status set planning
```

### Signal work is ready for PR review

```bash
openflows-harness status set review_ready
```

### Blocked

```bash
openflows-harness status set blocked
```

## After setting a phase

- `planning` → NEXUS spawns SENTINEL to review the plan (planning gate).
- `building` → implementation proceeds after SENTINEL gate approval.
- `review_ready` → NEXUS spawns SENTINEL to review the PR; SENTINEL's verdict is
  submitted via `openflows-harness review submit`.
- `blocked` → surfaced to NEXUS / human for intervention.
