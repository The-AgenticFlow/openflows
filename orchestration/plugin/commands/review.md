---
name: review
description: Submit a SENTINEL review verdict to the controller
---

# /review Command

Submit a review verdict for the current ticket. Run via the CLI:

```bash
openflows-harness review submit --verdict <approve|reject> --report <path-to-eval.md> [--pr <N>]
```

## Arguments

| Arg | Required | Description |
|---|---|---|
| `--verdict` | Yes | `approve` or `reject` |
| `--report` | Yes | Path to the evaluation markdown written during review (`segment-N-eval.md` / `final-review.md`) |
| `--pr` | No | PR number, if the verdict targets a specific PR |

## When to Use

After SENTINEL finishes reviewing a PR (or completed work):

- **`approve`** — the work earns its merge; the controller routes the ticket onward.
- **`reject`** — the work needs changes; the controller routes back to FORGE to rework
  in **its existing chat session**.

## What it does

Writes `ticket:{id}:review:sentinel:pr_review` (a `ReviewPayload` `{verdict, report, pr_number}`)
to SharedStore. This is the **PR/final review** verdict — it is namespaced by review type
(`pr_review`) so it never collides with the planning-gate review state
(`ticket:{id}:gate:planning`). The controller (SENTINEL node) reads this key to route the
ticket. This is the **machine-readable handshake** — the controller does **not** read
`STATUS.json` or the report file directly.

## Examples

### Approve a PR

```bash
openflows-harness review submit --verdict approve --report final-review.md --pr 42
```

### Reject with rework guidance

```bash
openflows-harness review submit --verdict reject --report segment-N-eval.md --pr 42
```

## Notes

- Keep inline `file:line` guidance in the report — FORGE reads it to address blockers.
- A `reject` loops back to FORGE in the same session; FORGE re-signals
  `openflows-harness status set review_ready` after addressing the report.
