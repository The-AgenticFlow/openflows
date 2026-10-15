# DoD-driven SENTINEL verification

SENTINEL selects verification evidence from the exact approved plan's Definition
of Done (DoD), ticket criteria, candidate implementation and repository conventions.
NEXUS testing dispatch and lifecycle hooks share strategy guidance, and the
registry provisions `sentinel-testing` alongside the existing review skills.
Neither dispatch nor the harness chooses checks by language or file extension.

On startup and orchestration reset, existing registries gain the required
`sentinel-testing` skill if their SENTINEL entry lacks it. The upgrade appends
only that skill, preserves role toggles and other settings, and leaves an absent
SENTINEL role absent. Repeated upgrades do not duplicate the entry.

Before requesting execution, SENTINEL records a strategy in `review.md`: each
criterion or regression risk, the observable evidence required, selected checks,
necessary dependencies and reasons omitted broader checks are unnecessary.
The completed report maps those items to inspection findings and observed A2A
results, or explicit unresolved gaps. A successful task is not automatically a
complete review.

Commands still execute through A2A in FORGE's clean temporary candidate checkout.
The harness requires exit-zero evidence with the recorded candidate HEAD and
uses the lifecycle version snapshot when recording it. Review decisions still
validate the approved revision, review round and HEAD. Rejection clears candidate
evidence and returns FORGE to building; new candidates require fresh verification.
Human PR approval and current-head CI remain merge requirements.

Missing tools require installation only when the selected check is necessary.
SENTINEL reconsiders an irrelevant command and obtains sufficient replacement
evidence without sending FORGE to install irrelevant dependencies. A required
compiler, test runner or other workspace dependency that cannot run remains an
actionable setup failure: reject to building with diagnostics and the unverified
criterion. Unavailable external credentials/services remain external blockers.

## Deliberate limits

The lifecycle requires successful **executable** A2A evidence. It has no
inspection-only approval mode. Content assertions are appropriate only when
they establish a real criterion or regression risk and detect a relevant defect.
Where inspection alone is meaningful, SENTINEL reports the limitation and asks
for clarification or an explicit plan revision instead of running `true`, `echo`
or another unrelated command. Clarification that changes the contract uses
FORGE's existing return to planning, new plan upload, and fresh plan approval.
Testing rejection alone retains the approved plan and cannot change its DoD.

The state stores a successful task and HEAD, not a structured ledger proving
coverage of every DoD item. Semantic sufficiency remains SENTINEL's responsibility.
Supporting inspection-only approval or machine-validated coverage would require
a separate evidence/protocol and lifecycle design; #391 does not weaken gates
to approximate either. Temporary-checkout build caching is also unchanged.

## Validation

`dod_verification` tests exercise meaningful content assertions without Cargo,
executable examples, Rust and non-Rust behavior, dependency diagnostics, and
missing/stale evidence rejection. Positive and defective candidates verify that
the checks can fail for the properties they claim. NEXUS tests cover exact
dispatch context, testing instructions in initial and reused reviewer sessions,
and registry/bootstrap availability of the testing skill. Existing lifecycle,
relay, and sandbox tests continue to protect authorization and execution behavior.

These automated tests validate wiring and execution, not live model judgment.
Use [the strategy evaluation fixtures](../../tests/evals/sentinel-testing.md) to
evaluate a deployed SENTINEL by evidence relevance and sufficiency. They include
the same document under different DoDs, broader integration risk, necessary
versus irrelevant tool failures, insufficient evidence and inspection-only work.
