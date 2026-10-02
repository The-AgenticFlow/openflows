# OpenFlows lifecycle design

Implement the supplied diagram as the authoritative durable lifecycle:
planning -> plan_ready -> building -> testing -> submit -> done.
Plan rejection enters plan_rejected and returns to planning; failed testing,
review, or pipeline checks return to building. Blocked may return to planning.

One typed, versioned lifecycle record contains the plan revision/content,
review decisions, tested head, verification evidence, PR and merge identity.
All lifecycle mutations use an atomic compare-and-set operation. Legacy
states are read conservatively: unversioned active work requires re-planning;
confirmed legacy merged records remain terminal. No old approval is inherited.

FORGE authors/submits the plan and implements. SENTINEL decides plan review
and testing compliance. Human approval is required for testing and submission,
as depicted. VESSEL merges only the submitted, approved, tested head after CI
success, with a GitHub expected-SHA precondition. Done requires merge evidence.
Changing the plan or returning to building invalidates downstream evidence.

Existing chat provisioning, A2A execution, PR discovery and recovery are reused.
A testing review job is distinct from plan and PR jobs. Decisions are durable;
notifications are retriable. Existing deployment scripts and template changes
in the user's checkout must remain untouched.

Validation: reducer graph/evidence tests; atomic concurrency and persistence
failure tests; hook tests; controller routing tests; CI timeout/head-race tests;
workspace test suite. No live merges/deployment during implementation.
