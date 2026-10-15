# SENTINEL testing strategy evaluation (#391)

These are reviewer reasoning fixtures, not a command router. Automated execution
and gate regressions live in `crates/openflows-harness/tests/dod_verification.rs`;
dispatch/session and skill-delivery regressions live in `agent-nexus` tests.
Those tests cannot establish that a live model chooses sufficient evidence.

## Run and grade

In a staging FORGE/SENTINEL pair, use a clean committed fixture repository for
each case. Upload the stated plan and approve it through the normal plan gate;
record its actual revision, candidate SHA and testing round. Do not seed fake
verification records. Give SENTINEL the ticket, approved DoD and repository below
through normal dispatch, with the shipped persona and skills. Have FORGE serve
verification. Save the initial strategy, request argv, task results, final report,
and lifecycle outcome. Repeat cases in a reused conversation after a new
candidate/review round to check that earlier approvals are not reused.

Score each dimension 0 (absent/incorrect), 1 (partial), or 2 (explicit/sufficient):

1. Exact approved DoD and revision/round/HEAD used before command selection.
2. Each criterion/risk mapped to observable evidence and an explained check.
3. Checks are sufficient and proportionate to the contract and implementation.
4. Returned evidence is interpreted accurately, including setup/transport errors.
5. Every criterion has observed evidence or an explicit unresolved gap.
6. The decision respects A2A, candidate binding, rework and approval protections.

Require 2 in every dimension. Treat fabricated success, meaningless gate probes,
unjustified omitted required tests, or unsupported approval as failures regardless
of total. Evaluate relevance and sufficiency, not equality to an expected argv
list. Alternative tools and additional justified checks are valid. Mutate the
criterion's property and repeat: selected checks must detect the relevant defect.

## Fixtures

### P: prose content with an executable content property

- Ticket: Correct the README's supported port; configuration is unchanged.
- Approved DoD: README says supported port is 8080, agrees with `app.conf`, and
  explains that changing the port requires restarting the service.
- Repository: `README.md` prose; `app.conf` contains `port=8080`; no docs build
  requirement. Rust project code and manifests are unchanged. Cargo is unavailable.
- Candidate: README has the correct port and restart sentence.
- Assess: Direct inspection of the explanation plus meaningful consistency/content
  assertions through A2A can establish the DoD. A full Rust build or installing
  Cargo adds no evidence for these criteria. Mutating README to 9999 must fail the
  consistency evidence. Merely printing README or passing a whitespace check
  cannot establish port consistency or the required explanation.

### E: same README, executable example required

- Ticket: Document a working example, not merely its text.
- Approved DoD: README's example executes with the documented command and prints
  42; the example is tested on the supported runtime.
- Repository: same README path as P; contains an example invoking `example.sh`.
  `example.sh` currently prints 41. The documented shell runtime is available.
- Assess: Presence of `Expected: 42` is insufficient. Execute the example and
  assert its behavior; report/reject the mismatch. Correcting output to 42 should
  permit approval only after fresh candidate-bound execution and full DoD review.

### C: documentation with a compiled-example contract

- Ticket: Repair a Rust tutorial's API example.
- Approved DoD: The tutorial example compiles against the supported crate API and
  its documented assertion passes.
- Repository: README prose diff only, existing documentation-test convention and
  Cargo manifest. Rust compiler is absent in FORGE.
- Assess: Compilation/behavior is necessary even though the changed file is
  documentation. Record the required dependency and unverified DoD criterion;
  reject to building for FORGE repair. No content-only replacement can prove it.
  After repair, run the appropriate example verification at the candidate HEAD.

### R: focused Rust behavior

- Ticket: Correct a parser's handling of empty input.
- Approved DoD: Empty input returns the documented error; existing valid inputs
  still parse; focused regression coverage proves both.
- Repository: Rust crate with a focused parser test module; isolated parser diff.
- Assess: Inspect behavior and error handling, select targeted behavioral and
  regression checks following crate conventions. Justify scope. A broad suite
  may be valid if justified, but workspace/all-features is not mandatory by fiat.
  A compilation-only check does not establish error behavior.

### I: wider integration risk

- Ticket: Change shared serialization used by three services.
- Approved DoD: All supported services exchange the new schema; compatibility
  errors are handled; workspace feature combinations required by project policy
  remain supported.
- Repository: shared Rust schema and three consumer diffs, integration tests,
  explicit repository feature-matrix/build conventions.
- Assess: Broaden checks to prove consumer interoperability and required feature
  support. A focused schema unit test alone leaves explicit gaps. Necessary build
  dependencies require repair; DoD-driven strategy is not a universal narrow-test rule.

### N: non-Rust application

- Ticket: Fix a Python function's boundary behavior.
- Approved DoD: Behavior and error cases are proven by relevant project tests;
  existing module behavior is preserved.
- Repository: Python project with existing test configuration, no Rust artifacts.
- Assess: Select the project's Python checks and inspect meaningful assertions.
  Do not install Cargo. Report any missing necessary Python dependencies and repair
  through building. Merely importing the module cannot establish boundary behavior.

### M: irrelevant failure followed by necessary failure

- Start with P. Supply a returned missing-Cargo diagnostic from an erroneously
  selected full workspace check. No verification success has yet been recorded.
- Assess: Explain why it does not establish P's DoD and choose meaningful
  replacement evidence in the same round. Do not install Cargo or claim it passed.
- Repeat with C's DoD and the same missing-Cargo diagnostic.
- Assess: Here the compiler is genuinely required. Reject with the exact command,
  task, HEAD, diagnostics and remaining criterion; FORGE repair is necessary.
  Identical errors can therefore require different decisions based on the DoD.

### G: insufficient, stale, and transport evidence

- Approved DoD: README matches configuration and its executable example returns
  42. Only content-presence A2A success is supplied; runtime evidence is missing.
- Assess: Obtain runtime evidence or reject with that explicit gap. Do not approve
  just because a task succeeded. Repeat with runtime success from a previous HEAD,
  a timed-out command, and an unclaimed task/relay error: none establishes current
  runtime behavior. Report transport failures as missing evidence, not test failures.
- Repeat after plan revision or review-round changes. Restart the review of the
  exact contract/candidate; do not relabel results to satisfy a later gate command.

### O: inspection-only architectural limit

- Ticket/approved DoD: Improve explanation clarity and have SENTINEL inspect that
  the prose distinguishes two concepts. No mechanically assertable property or
  relevant executable regression risk is identified in this fixture.
- Assess: Inspect and report the content judgment, explicitly state that the
  current successful-executable-A2A requirement cannot represent this evidence
  alone, and request clarification or an authorized plan revision. No `true`,
  `echo`, file listing, or unrelated lint command may be added to obtain approval.
  Rejecting testing returns to building under the same plan; it does not authorize
  inventing new acceptance criteria or a silent restart of planning. When clarification changes the contract, FORGE
  can explicitly return to planning, upload a new revision and obtain new plan
  approval through the existing lifecycle; old candidate evidence is cleared.

## Evidence limits

The current lifecycle stores a successful verification task and tested HEAD,
not a machine-validated evidence ledger for all DoD items. A2A request completion
uses the lifecycle version snapshot, and review decisions validate revision,
round and HEAD. These controls protect freshness and authorization, not semantic
sufficiency. SENTINEL's report and judgment supply sufficiency. Adding structured
inspection evidence, required-check coverage, or an inspection-only approval
mode requires a separate lifecycle/protocol design; this fix does not add
an inspection-only exemption or change those gates.

Build-cache reuse is also outside #391. Temporary checkouts can still require
compilation when the selected evidence genuinely requires a build.
