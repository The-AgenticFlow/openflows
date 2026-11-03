# OpenFlows End to End Testing Architecture

The goal is to prove that a new OpenFlows build can take an issue, provision agents, produce a correct change, verify the exact commit, obtain the required approvals, merge that change, and clean up. A passing test must include independent evidence of the resulting code and repository history. An agent saying it completed the task is insufficient.

The connected system scenario combines service integration, delegated verification, external-service fixtures, and production bootstrap into one issue-to-merge journey. Recovery, onboarding, and live scenarios remain future work. Tests reduce regression risk for the scenarios they cover; they cannot guarantee that every possible change is safe.

## The test layers

| Layer | Environment | When it runs | Status |
| --- | --- | --- | --- |
| Build and unit tests | Rust workspace | Every PR and merge candidate | Existing |
| Service integration | Real Redis and PostgreSQL | Every PR and merge candidate | Covered by service and manager integration tests |
| Workspace integration | Real Coder, Terraform, SSH, workspaces, Redis, A2A | Every PR and merge candidate | Covered by workspace and delegated verification tests |
| Connected deterministic system E2E | Real OpenFlows and Coder; local GitHub and model fixtures; real Git | Every PR and merge candidate | One complete scenario; recovery/rework cases remain |
| Live external E2E | Production templates, linked GitHub authentication, disposable GitHub repo, real model provider | Nightly and before release | Future implementation |
| Manager and onboarding E2E | Manager HTTP API, PostgreSQL, tenant configuration, Coder; browser journey where applicable | Relevant PRs initially; mandatory once stable | Future implementation; current manager integration is a foundation |

The deterministic system suite provides repeatable merge decisions without external credentials. The live suite checks the external contracts, credentials, templates, and model behavior that local fixtures cannot establish. A required live release check must actually run and pass for that candidate; missing credentials cannot silently turn it into a passing skipped job.

## Complete deterministic system topology

Each run creates a fresh Docker network and disposable resources. Coder creates the Nexus and role workspaces. The controller inside the Nexus workspace runs the production flow graph, including NEXUS scheduling and VESSEL merge handling, and hosts the hook consumer and A2A relay.

```mermaid
flowchart TB
    Driver["Scenario driver: issue input, human actions, assertions"]
    GH["Local GitHub API fixture"]
    Git["Real Git remote and repository"]
    CI["Independent candidate test runner"]
    Model["Scripted model HTTP provider"]
    Coder["Real Coder server and chat engine"]
    PG["PostgreSQL: Coder and manager databases"]
    Nexus["Nexus workspace: real OpenFlows controller, hooks, A2A, VESSEL"]
    Redis["Real Redis: tenant state and audit"]
    Forge["Real FORGE workspace"]
    Sentinel["Real SENTINEL workspace"]
    Manager["Real manager API for onboarding scenarios"]

    Driver --> GH
    Driver -->|"Human approval through public interface"| Nexus
    Driver -->|"Independent checks"| Git
    Nexus <--> GH
    Nexus <--> Redis
    Nexus -->|"Provision workspaces and create chats"| Coder
    Coder <--> Model
    Coder --> Forge
    Coder --> Sentinel
    Coder -->|"Actual signed lifecycle hooks"| Nexus
    Coder --> PG
    Sentinel -->|"A2A verification request"| Nexus
    Nexus -->|"FORGE claims verification task"| Forge
    Forge <--> Git
    CI -->|"Test candidate checkout"| Git
    CI -->|"Publish checks for tested commit"| GH
    GH -->|"Perform requested merge"| Git
    Driver --> Manager
    Manager --> PG
    Manager --> Coder
```

The manager participates in onboarding scenarios. A controller scenario may begin with an already configured disposable tenant, so a manager failure and a controller failure can be diagnosed separately. The complete product journey combines onboarding with issue processing after those individual journeys pass.

## What is real and what is controlled

| Component | Test behavior |
| --- | --- |
| OpenFlows | Build from the candidate commit; run the actual controller, role routing, harness, hooks, A2A, and VESSEL code |
| Coder | Pinned real server, PostgreSQL, Terraform provisioning, chat engine, command execution, and workspace lifecycle |
| Workspace commands | Actually edit files, commit changes, run tests, request verification, and record reviews |
| Redis and PostgreSQL | Real servers and normal migrations; isolated tenant data and databases |
| Git repository | Real commits, branches, remote pushes, and merge operations |
| Model provider | Local HTTP fixture returning scripted text and tool calls; Coder must execute those calls in real workspaces |
| GitHub service | Local HTTP fixture implementing the required issue, PR, review, check, and merge endpoints; merge requests operate on the real Git repository |
| Candidate CI | Real acceptance commands run independently against the requested candidate; check status belongs to that commit |
| Human | Scenario driver performs the human approval through the supported CLI or API, including revision, round, and head |

The model fixture controls the agent's choices, not the results of its commands. It cannot write successful verification records, approve gates through Redis, or declare a merge complete. Unexpected tool schemas and unsupported GitHub requests fail with diagnostics rather than receiving generic success responses.

The GitHub fixture is a deliberately limited adapter, not a complete GitHub implementation. Its expected behavior must be checked against the live suite. Production GitHub external authentication and OAuth linking are separate live-test responsibilities; injecting a disposable fixture token does not prove that linking works.

## The first complete scenario

Use a small repository with a committed acceptance test and a known failing implementation. For example, an addition function returns the wrong result, and the independent acceptance test requires `add(19, 23)` to return `42`. The acceptance oracle is fixed by the scenario, independently of model responses.

1. The driver creates an issue through the GitHub fixture. NEXUS discovers it through the normal issue polling path.
2. NEXUS provisions FORGE and starts a real Coder chat. The scripted model inspects the repository and uses real tools to write and submit a plan.
3. The controller starts SENTINEL plan review. SENTINEL approves the current plan revision and round through the harness. FORGE cannot implement before that approval.
4. FORGE edits the implementation, commits, and enters testing with a clean candidate checkout.
5. SENTINEL requests acceptance verification through A2A. FORGE executes the command in a temporary checkout of the recorded head. The result must report the correct commit, executor, working directory, exit code, and output.
6. SENTINEL approves testing for that exact candidate. FORGE enters submit, pushes the branch to the real local remote, creates a PR through the fixture, and records it through the harness.
7. SENTINEL reviews the PR, and OpenFlows delivers the review through its normal GitHub path. The independent CI runner tests the PR head and publishes its check result.
8. Without human PR approval, VESSEL must leave the PR unmerged. The driver then approves the exact candidate through the public operator interface.
9. VESSEL requests the merge. The fixture checks the requested head against the current PR head and performs the configured Git merge operation.
10. The driver independently checks the resulting default branch, reruns the acceptance test, checks the commit relationship appropriate to the merge method, confirms lifecycle completion through the public interface, and confirms workspace cleanup.

Human PR approval is part of this scenario. Human testing approval is currently deferred in the lifecycle implementation and must not be presented as an existing required gate. The test should follow the current contract and change when that contract changes.

The driver may configure initial accounts and fixtures, create issues, perform human decisions, observe public interfaces, and inject declared failures. Agents and the controller must perform the remaining workflow. Directly writing lifecycle transitions or success outcomes to Redis would bypass the behavior this complete scenario is meant to test.

## Required failure scenarios

Add these after the first complete scenario works, one scenario at a time.

| Failure | Required outcome |
| --- | --- |
| Plan rejected | FORGE receives feedback; implementation stays gated until a revised plan is approved |
| Acceptance fails | No successful verification evidence or testing approval; FORGE can repair and obtain fresh verification |
| PR head changes after verification or approval | Previous evidence cannot authorize the changed candidate; fresh review and verification are required |
| Current round with old head; old round with current head | Each stale identity is independently rejected |
| Missing, failed, or pending CI | No premature merge; a pending check is not success |
| Human approval absent or rejected | No merge; rejection is reflected in the workflow |
| Repeated polling, duplicated hooks, or repeated delivery | No duplicate assignment, workspace, review, or merge side effect |
| Workspace or controller restart | Reconciliation preserves the candidate and required approvals without bypassing gates |
| Merge response lost after remote merge | Reconciliation detects the actual remote result without producing a second merge |
| Invalid hook signature or wrong A2A pair token | Request is rejected; lifecycle does not advance |
| Concurrent tenants | Tickets, credentials, verification, and approvals stay within their tenant |

Existing focused tests cover parts of these cases. Full system scenarios verify that the controller and transports preserve the same rules when combined.

## CI enforcement and diagnostics

The focused jobs and `Connected issue-to-merge E2E` feed the required `OpenFlows Merge Gate`. Run them against the candidate commit and the merge-queue candidate where a queue is configured. The final gate must reject failed, cancelled, timed-out, and skipped mandatory jobs.

Every run uses pinned dependencies, bounded startup and scenario deadlines, readiness checks, unique resource names, disposable credentials, and cleanup restricted to its own resources. CI scenarios must not load operator configuration or production volumes. Cleanup runs on success, failure, and cancellation.

Save correlated controller logs, Coder logs and chat tool results, authenticated hook event metadata, A2A results, candidate CI output, GitHub fixture requests, repository refs, approval identities, and the final acceptance result. Remove credentials from diagnostics. A green run must include positive evidence that the required hooks, verification, approval, and merge steps occurred; silence from a fail-open dependency is insufficient.

Do not hide intermittent failures with blanket reruns. Report startup failures separately from assertion failures, then repair the cause or keep the affected gate failing.

## Remaining coverage and completion criteria

1. Expand recovery, rework, approval, and tenant-isolation scenarios.
2. Add the manager onboarding journey through its public interfaces.
3. Add a live suite against a disposable GitHub repository and real model provider, using production templates and linked authentication. Run it nightly and against release candidates.

The deterministic E2E scenario must take an issue through the actual controller and agent paths to a real merged Git commit, independently pass acceptance, respect the required gates, and leave the expected cleanup state. The live suite must additionally verify production authentication and external services. Focused service tests alone cannot establish either result.

## External-service fixtures

The [system fixtures](../../tests/e2e/system/README.md) implement the local scripted model provider, limited GitHub API adapter, real Git remote, and independent exact-commit acceptance runner. Their mandatory contract job checks the production OpenFlows GitHub client and verifies actual acceptance failure, stale evidence rejection, and Git merging. The model contract checks normal and streamed tool responses and matching tool results.

The fixture contract driver deliberately controls Git/API calls and tests those adapters in isolation. The separate connected scenario uses real controller-created Coder chats and signed hooks; only the human and independent CI actions are supplied by its driver.

## Production bootstrap coverage

The production bootstrap scenario runs the public tenant CLI against a fresh Coder deployment. It uploads all five production Terraform templates, provisions Nexus, clones the real fixture repository, checks the Rust controller's health, and makes a real Coder chat execute a workspace command. It also checks that unchanged bootstrap reuses template versions, changed Terraform settings produce new versions, and workspace deletion succeeds. Logs are collected before deletion.

This check catches regressions in fresh tenant configuration, template upload and configuration forwarding, Nexus startup, repository cloning, and Coder command execution. It does not yet prove issue discovery, FORGE/SENTINEL chats, signed hook delivery, human approval, or VESSEL merging. Those are covered together by the connected scenario below.

## Connected issue-to-merge coverage

Run `bash tests/e2e/system/bootstrap.sh system`. The fresh tenant discovers an issue through the local GitHub API, provisions FORGE and SENTINEL with production templates, and uses actual Coder chats and phase hooks to plan, implement, commit, verify through A2A, and open/review a PR. The driver approves through the public operator CLI; actual VESSEL requests a real squash merge. Independent acceptance checks both the candidate and merged commit. The controller must mark the ticket merged and delete both role workspaces.

The scenario observes no merge attempts while human approval is missing or CI is pending. It also checks the verification executor, temporary checkout, tested head, unchanged default branch before approval, merged tree and parent, hook delivery, and cleanup. Evidence is uploaded by the required `Connected issue-to-merge E2E` job, which feeds `OpenFlows Merge Gate`.

The scripted model supplies repeatable tool choices; it does not test reasoning quality. The GitHub adapter exercises real Git but does not replace a live authentication or branch-protection test. Existing focused suites retain failed acceptance, stale approval, and rejected CI cases. Whole-journey recovery, rejection/rework, and tenant-concurrency scenarios still need expansion.

## Implementation references

- [Current container suites](../../tests/e2e/README.md)
- [CI workflow](../../.github/workflows/ci.yml)
- [Controller entrypoint and routing](../../binary/src/bin/agentflow.rs)
- [Ticket lifecycle and approval contract](../../crates/config/src/lifecycle.rs)
- [Delegated verification test](../../binary/tests/delegated_verify_e2e.rs)
- [System architecture](openflows-system-architecture.md)
