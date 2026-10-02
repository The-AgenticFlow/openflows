# State machine implementation plan

Goal: enforce the lifecycle and recovery loops in the supplied diagram.
Spec: ../specs/2026-09-29-state-machine-design.md
Execution: inline in the existing checkout; preserve unrelated user edits.
Architecture: config lifecycle reducer, SharedStore atomic persistence, harness
commands, controller/agent routing and current-head merge enforcement.

## Global constraints
- Required states: planning, plan_ready, plan_rejected, building, testing, submit, done.
- Human decisions required at testing and submit; blocked returns to planning.
- No silent persistence failure or inheritance of unverifiable legacy approvals.
- No live deployment, PR merge, or changes to existing user-edited files.

## Review focus
Concurrent transitions; stale approvals; interrupted external delivery; missing CI;
legacy mixed-format status and raw Markdown plans.

## Tasks / progress ledger
- [x] 1. Add typed lifecycle and event reducer in config/src/lifecycle.rs.
  Test legal flow, rejected plans, invalid edges, stale revisions, evidence,
  human gates, merge identity, terminal protection and legacy decoding.
- [x] 2. Add SharedStore lifecycle read/apply with Redis CAS and memory lock.
  Test competing updates and retries; preserve Redis errors.
- [x] 3. Route harness status/plan/gate/review/verify/PR commands through reducer.
  Add human gate CLI and revision/head arguments; preserve read compatibility.
- [x] 4. Update NEXUS, FORGE, SENTINEL and hooks for explicit planning readiness,
  testing review, decisions/rework and consistent terminal state.
- [x] 5. Enforce submitted approvals, current-head CI and SHA-conditional merge
  in VESSEL/GitHub; timeout or missing checks cannot authorize merge.
- [x] 6. Update agent instructions, run suites and independent code review.

Ruling: work in place to preserve the user's active checkout and existing edits.
Ruling: prior diagram/audit plus explicit go-ahead constitute implementation approval.
Ruling: public CLI role labels are trusted within the existing Redis/operator trust
boundary; no new authentication infrastructure is introduced by this change.

Implementation ledger:
- Lifecycle reducer, round-bound decisions, durable history and CAS implemented.
- Harness, controller routing, A2A head evidence, hooks and operator human gates implemented.
- PR delivery moved into the lifecycle record to avoid lossy dual writes.
- Merge uses persisted reservation and GitHub expected SHA; uncertain outcomes remain reserved.
- First independent review found merge-reservation and stale-round races; both addressed with regression tests.
- Follow-up independent review unavailable because reviewer service hit its usage limit; final integration review is local.
- Real Redis integration passed concurrent CAS, Markdown round trip and injected OOM (no state change).
- Workspace suite passed 333 tests before subsequent CI aggregation/migration regressions were added; final runs pending below.

Final verification (2026-09-29):
- cargo test --workspace --all-targets --all-features: 337 passed, 0 failed; disposable Redis test ignored here and run separately.
- bash tests/integration/gated_workflow_test.sh: 1 passed, including CAS races and injected Redis OOM.
- cargo clippy --workspace --all-targets --all-features -- -D warnings: passed.
- Conflict handling now transitions Submit to Building before mutating a worktree; unresolved merges stay frozen.
- Live Coder/GitHub deployment was not exercised; rollout and legacy replan requirements are documented.
