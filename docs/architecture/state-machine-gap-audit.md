# State-machine gap audit

Date: 2026-09-29. Scope: supplied target diagram versus the current checkout. This is a source audit and proposed remediation sequence, not an implementation or production incident diagnosis.

## Interpretation of the target

The diagram separates agent activities from durable shared-store states:

`planning → plan_ready → building → testing → submit → done`

Plan rejection branches from review to `plan_rejected`, then returns to planning. Testing reviews the implementation against the approved plan, using SENTINEL, A2A verification, and human involvement. Failed testing or pipeline checks return work to building. Successful pipeline checks and the required PR review permit merge; confirmed merge produces `done`.

Interpretation: `plan_ready` means submitted for review, not already approved. Approval authorizes entry to building. Whether humans must approve both Testing and Pull Request, and whether a draft PR may exist before `submit`, remain policy decisions. The drawing does not specify retry limits, blocked/cancelled states, deadlines, or administrative reopening.

## Mapping to current implementation

| Target | Current code | Assessment |
| --- | --- | --- |
| Planning | FORGE uploads a plan and sets `planning`; NEXUS spawns a planning reviewer | Implemented, but drafting and review readiness are conflated |
| `plan_ready` and plan review | `planning` plus `ticket:{id}:gate:planning` | Partial; no explicit submission event, revision, or durable review decision |
| `plan_rejected → planning` | Reviewer prompt says provide feedback and do not approve | No first-class rejection transition or reliable rejection delivery loop |
| Building | `building` | Present; ordering and approval invariants are incomplete |
| Testing against approved plan | `testing`, A2A verification commands, later PR review | Capabilities exist; no enforced testing exit gate |
| Human participation | Chat `RequiresAction`, ticket `AwaitingHuman`, GitHub reviews | Partial mechanisms; no explicit two-stage human approval policy |
| `submit` / Pull Request | PR metadata plus `review_ready` | PR already exists when final SENTINEL review starts |
| Pipeline failure → building | VESSEL CI fix and review rework dispatch | Substantial recovery machinery exists; not a unified phase transition |
| Merge → `done` | VESSEL merge, ticket `Merged`, status string `"Merged"`, deployment record | Completion exists in several representations, not one terminal phase |

Relevant implementations: `crates/openflows-harness/src/store.rs`, `crates/config/src/state.rs`, `crates/agent-nexus/src/lib.rs`, `crates/agent-sentinel/src/lib.rs`, and `crates/agent-vessel/src/node.rs`.

## Findings, in priority order

### 1. Plan approval does not actually prevent implementation

`crates/agent-nexus/src/hooks/guard.rs:624` denies source writes in planning only when the plan is absent. Once the plan exists, source writes are allowed without checking approval. `crates/agent-nexus/src/hooks/slice_tests.rs:196` explicitly tests and accepts this behavior.

Failure sequence: upload plan → remain in planning with no approval → edit source successfully through the hook. Blocking `status set building` alone does not block building activity.

Required outcome: source changes remain prohibited while drafting/submitted for review, except permitted planning artifacts; implementation begins only after the approved revision enters building.

### 2. The harness validates phase names, not the transition graph

`crates/openflows-harness/src/store.rs:21`, `:129`, and `:185` implement a valid-name list, entry check, and approval only when leaving planning. There is no complete edge table.

Examples permitted by this logic:

- `planning → review_ready` with one planning approval: skip both building and testing.
- `building → review_ready`: skip testing entirely.
- First state `blocked`, then `blocked → building`: bypass planning at the store API. The blocked hook restricts this, but the authoritative writer does not.
- `planning → blocked` requires approval: a worker cannot report a blocker from planning before review succeeds.

Required outcome: one transition authority validates source state, target state, actor, evidence, and permitted recovery edges. Hook checks should reflect that authority.

### 3. State encoding changes between writers

The harness writes `{phase, role, ts}`. SENTINEL approval writes `"approved"` to the same key (`crates/agent-sentinel/src/lib.rs:506`). VESSEL writes `"Merged"` (`crates/agent-vessel/src/notifier.rs`, `set_ticket_status_merged`). FORGE's typed reader and the hook reader expect an object with `phase`.

Consequences: approval/completion becomes unreadable to some consumers; the harness interprets string status as no phase; hooks see an unknown/unset phase and permit actions under their fallback. Terminal-state protection is absent from the harness.

Required outcome: one versioned lifecycle record; ticket scheduling and worker allocation may remain separate projections, but must not overwrite the lifecycle format.

### 4. State writes can silently fail, and gate consumption is not a transaction

`status_set` performs GET → GETDEL approval → SET status. GETDEL makes consumption single-use, but does not atomically validate and update the state. A failed final SET loses the approval while leaving the ticket in planning. A concurrent replan can also race with a writer using an old phase snapshot.

Many harness mutations discard Redis SET errors, then print success: status, gate approval, plan, handoff, PR, review, and merge records. The general SharedStore Redis backend also discards write errors (`crates/pocketflow-core/src/store.rs:117`).

Required outcome: return persistence failures; atomically validate expected lifecycle version, consume applicable approval, update state, and record the transition. Use idempotent event IDs for retries and a durable delivery mechanism for external effects.

### 5. Plan rejection cannot reliably drive a revision cycle

NEXUS tells the planning reviewer to provide feedback and withhold approval (`crates/agent-nexus/src/lib.rs:2237`). Gate commands offer approve/status, without reject. A waiting reviewer with no approval is treated as orphaned and its binding cleared (`:2071`), which cannot distinguish rejected, waiting for clarification, and abandoned reviews.

Likely failure: reviewer rejects the plan in chat, FORGE remains waiting for approval, and NEXUS starts another reviewer. The exact production frequency is not established by this audit.

Required outcome: a durable decision with verdict, feedback, plan revision, reviewer, and timestamp; deliver rejection to FORGE idempotently; distinguish genuine waiting from failed agents; resubmit a new revision explicitly.

### 6. Approvals and test evidence are not bound to the artifact being approved

`GateApproval` contains phase, role, timestamp, and notes, but no plan digest/revision. `plan_write` can overwrite the plan without invalidating approval. `ReviewPayload` has verdict/report/optional PR number, but no commit SHA. `status_set` does not require a test or A2A result. NEXUS spawns SENTINEL for planning and review_ready, not testing; `sentinel_job` likewise has no testing job.

Required outcome: approval references the exact plan revision; testing approval references that plan and the tested commit; PR approval and CI success reference the merge candidate. Changed artifacts invalidate dependent evidence.

### 7. Pipeline timeout can still lead to a merge attempt

`crates/agent-vessel/src/node.rs:1685` handles CI timeout by checking reviews with a supplied `CiStatus::Success`, then calls the merger if review conditions allow. It can return `CiMissing` after a successful merge. Thus successful CI is not an unconditional application-level prerequisite. Repository protections may independently reject the attempt; those settings were not inspected.

Required outcome: distinguish passed, failed, pending, timed out, and intentionally not configured. Timeout must not imply success. Any no-CI exception needs an explicit project policy.

### 8. Merge reviews are not tied to a required actor or an exact head

`effective_review_state` (`crates/github/src/rest.rs:1583`) aggregates latest reviews by user; it accepts any approval in the absence of changes requested. It does not verify SENTINEL identity or a separate human approval. `PrMerger` calls a merge API whose request contains no expected head SHA (`crates/github/src/rest.rs:589`, `:1408`). CI polling uses the initially supplied head SHA.

Failure risk: after checks or approval, the branch changes and the application attempts to merge the new head using evidence about the old one. GitHub branch protection may mitigate this, but application code does not enforce it.

Required outcome: explicit reviewer policy, current-head evidence, and merge with expected SHA; a head change returns the candidate to verification.

### 9. Review delivery failures can strand an internally approved ticket

SENTINEL writes approved status, attempts the GitHub review non-fatally, deletes the verdict, and releases its slot (`crates/agent-sentinel/src/lib.rs:506–561`). If the GitHub review fails, VESSEL may keep waiting for approval while the verdict is gone and the phase no longer triggers review_ready processing.

Required outcome: preserve the durable decision and retry its delivery until acknowledged. Review completion and delivery completion are distinct facts.

### 10. Plan storage has an inconsistent serialization contract

Harness `plan_write` writes raw Markdown (`crates/openflows-harness/src/store.rs:816`), whereas NEXUS reads it through `get_typed::<String>` (`crates/agent-nexus/src/lib.rs:2241`); SharedStore parses Redis values as JSON first (`crates/pocketflow-core/src/store.rs:105`). Ordinary Markdown therefore becomes None in the NEXUS dispatch payload.

Important scope: hook plan existence already uses key presence to avoid this issue (`crates/agent-nexus/src/hooks/context.rs:155`), and harness `plan read` reads raw text correctly. This is an incomplete dispatch artifact, not proof that all plan reviews fail.

Required outcome: a shared typed plan envelope with content, revision, digest, and submission metadata; migrate legacy raw values.

### 11. Role checks are caller-supplied at the store boundary

Harness role comes from `OPENFLOWS_ROLE` (`crates/openflows-harness/src/main.rs:249`); approval checks compare that string to sentinel. This is a useful accidental-misuse check, but does not authenticate a reviewer. Workers connect directly to Redis. Hooks provide additional checks, but are not equivalent to authorizing every state mutation at a service boundary.

Required outcome: trusted actor identity and ticket scope on transition requests, with restricted direct lifecycle writes. The existing A2A authentication can inform that design; it does not currently authorize harness Redis mutations.

## Existing mechanisms worth preserving

- Separate planning-review and PR-review namespaces prevent chat/verdict collisions.
- Initial downstream phase rejection and single-use planning approvals provide a starting point.
- A2A verification includes bounded polling, result retrieval, timeout handling, and execution-result checking.
- SENTINEL PR rejection persists rework and returns the phase to building.
- VESSEL monitors reviews, conflicts, and CI; dispatches targeted rework; and reconciles already-merged PRs.
- Phase-aware hooks and unit tests provide a useful enforcement surface once aligned with the target contract.

## Proposed remediation sequence

These are planning work packages; implementation details should follow agreement on the lifecycle semantics.

1. **Define the contract.** Specify every edge in the diagram, its actor, required evidence, failure/retry behavior, and human policy. Keep blocked/awaiting-human as explicit operational states or orthogonal fields. Define what `done` means and how external merges reconcile.
2. **Establish the authoritative record and transition operation.** Add a typed phase enum and versioned lifecycle record. Make all controller/harness writers use one atomic operation. Standardize plan serialization and propagate storage errors. Provide a migration for object/string statuses and raw plans.
3. **Complete the planning loop.** Separate drafting from plan_ready; persist approve/reject decisions tied to revisions; deliver feedback and resubmissions reliably; block source edits until approval. Remove orphan heuristics that equate nonapproval with failure.
4. **Make testing a real gate.** Define required local/A2A evidence and plan-compliance review. Tie evidence to a commit and plan revision; record any required human decision. Permit submit only after this gate succeeds; rejection returns to building.
5. **Harden submission and merge.** Require the configured reviewers and successful required checks for the current head. Persist delivery retries. Disallow timeout-as-success. Merge with the expected SHA and record done only after a confirmed merge; reconcile external merges separately.
6. **Exercise recovery and concurrency.** Verify duplicate events, concurrent transitions, crashes between effects, Redis failures, stale plan approvals, head changes, reviewer absence, and rejected/retried work. Confirm all status projections converge after restart.

## Acceptance scenarios

- Happy path visits all required stages; done records PR and merge SHA.
- A plan without approval cannot authorize source edits or any downstream transition.
- Rejection persists feedback, returns to planning, and allows a new review without respawn churn.
- Revising an approved plan invalidates the previous approval.
- Failed/missing testing evidence prevents submit; failed CI returns work to building; timeout waits/escalates.
- A new commit invalidates old test/review/CI evidence and cannot slip into a merge race.
- Redis write failure produces failure, preserves recoverability, and never reports success.
- Concurrent or duplicate events result in one valid transition and one durable decision.
- Approved and merged records remain readable by every consumer; done cannot silently regress.
- Human requirements, once chosen, are enforced independently of chat waiting states.
- Real Redis plan round trips and transition tests complement in-memory tests.

## Verification scope

Source paths and failure sequences above were traced in the current checkout. No Coder/GitHub production workflow or live Redis mutation was exercised. Existing integration script `tests/integration/gated_workflow_test.sh` is primarily a command smoke test: it accepts some error output and cannot establish lifecycle ordering or recovery correctness. Test execution results are recorded separately below when available.

- `cargo test -p openflows-harness --lib --locked`: 11 passed, 0 failed.
- `cargo test -p agent-nexus --lib phase_guard_allows_source_write_once_plan_exists --locked`: 1 passed, 113 filtered out. This confirms the existing hook permits source writes with an uploaded plan while still in planning; it does not establish target compliance.
- `git diff --check`: passed. Application source was not edited; this audit document is the only intentional addition. The three pre-existing modified files were left intact.
