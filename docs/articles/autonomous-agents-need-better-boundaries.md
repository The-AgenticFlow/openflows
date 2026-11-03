# The Agent That Deleted Production Was Not the Root Cause

*Autonomous agents need least privilege, separation of duties, and independent review—not the full authority of a human developer.*

An autonomous coding agent deletes a production database from a developer's machine.

Months later, someone opens an issue: add a safeguard, improve the prompt, prevent the agent from running that command again.

By then, the most important question has already been missed.

Why did a coding agent on a developer machine have credentials capable of deleting a production database in the first place?

The immediate failure may have been an incorrect command. The systemic failure happened earlier, when the infrastructure gave one runtime enough authority to build, approve, and operate production without meaningful boundaries.

Autonomous agents are not inherently the problem. But autonomy amplifies the design of the environment around it. A carefully constrained agent can be useful and predictable. An agent with a developer's complete credentials, broad network access, and no independent reviewer is simply an unbounded production principal that happens to communicate in natural language.

That is not an agent architecture. It is a permissions accident waiting for an incident report.

## Human access is not an agent permission model

The easiest way to deploy an engineering agent is to give it the same environment as a developer: the developer's shell, GitHub session, cloud credentials, Kubernetes context, password manager, and access to production.

It is also the most dangerous way.

A human stakeholder may legitimately carry several responsibilities. A senior engineer can write code, review an emergency change, operate infrastructure, and communicate with the organization. Those powers are moderated by context, social accountability, fatigue, judgment, and organizational process.

An agent runtime should not inherit that complete role merely because it runs on the human's machine.

The correct question is not, “What can the developer do?” It is, “What is the smallest set of actions this agent needs for this specific responsibility?”

The answer should be encoded in infrastructure, not left to a system prompt.

## The builder must not be both judge and jury

The most important boundary is not between humans and agents. It is between responsibilities.

An agent that writes a change should not be the only agent that decides whether the change is correct. It has the same context, assumptions, and blind spots that produced the implementation. Asking it to review itself often produces a more articulate version of its original reasoning, not an independent assessment.

The builder can and should run tests. It should inspect its diff, check formatting, and catch obvious mistakes. But those checks are evidence supplied to a review process; they are not the final verdict.

Independent validation matters for the same reason that independent code review matters in a human organization. The reviewer must be able to reject the work. The rejection must return the work to the builder. The builder must not be able to approve its own plan, manufacture its own review decision, or bypass a failed gate.

In other words: an agent should never be both the judged and the jury of its own work.

## How OpenFlows separates authority

OpenFlows divides the software-delivery lifecycle among agents with distinct responsibilities:

- **NEXUS** coordinates work, assigns tickets, recovers stalled flows, and handles human escalation.
- **FORGE** plans and implements changes on an assigned branch.
- **SENTINEL** independently reviews plans, runs verification against the builder's exact candidate commit, and approves or rejects the result.
- **VESSEL** evaluates merge readiness, CI, conflicts, and deployment conditions before merging or operating infrastructure.
- **LORE** handles documentation work through its own bounded repository profile.

These are not decorative personas. Each role receives a different permission profile and occupies a different position in an enforced state machine.

The normal lifecycle is:

```text
planning -> plan_ready -> building -> testing -> submit -> done
```

FORGE can submit a plan, but it cannot approve that plan and move itself into implementation. SENTINEL's approval performs that transition. FORGE can produce a candidate commit, but SENTINEL verifies and approves the exact revision and commit before the flow enters submission. VESSEL merges only after the required review and current-head CI evidence are present.

Rejection is a real state transition, not a comment that the builder is free to ignore. Frozen phases prevent source changes while a candidate is being reviewed. A blocked flow recovers through planning instead of jumping directly back into implementation. Review rounds and commit identities are recorded so an approval for an old head cannot silently authorize a new one.

This matters because prompts are advisory. State transitions, credential boundaries, and denied operations are enforceable.

## Least privilege must exist at token issuance

OpenFlows does not hand every workspace a broad GitHub credential and ask the agent to use it carefully.

The Manager acts as a credential broker. It knows the workspace, tenant, repository, installation, and stored agent role. A runtime may request only a coarse purpose—Git transport or API access. It cannot choose its own repository or submit a custom permission set. The Manager resolves a server-controlled, versioned profile. Unknown roles fail closed.

The current profiles are deliberately different:

| Agent | Git access | API access | Intended responsibility |
|---|---|---|---|
| NEXUS | Contents: read | Contents: read; issues and pull requests: write | Discover and coordinate work without implementing repository changes |
| FORGE | Contents and workflows: write | Contents and pull requests: write | Implement, push the assigned branch, and create or update its pull request |
| SENTINEL | Contents: read | Contents and checks: read; pull requests: write | Inspect code and CI, then submit independent review feedback |
| VESSEL | Contents: write | Contents and pull requests: write; checks, statuses, and actions: read | Perform approved merge and delivery operations using current CI evidence |
| LORE | Contents: write | Contents and pull requests: write | Prepare and submit documentation changes |

There is no wildcard profile. Repository administration is not part of the runtime permission envelope. GitHub's implicit metadata read permission is accepted, but surplus grants are rejected when validating a token profile.

Even exceptional access is narrow. FORGE receives workflow write permission only for Git operations because GitHub requires it when a legitimate change modifies a workflow file. That capability is not added to its API token, and it is not inherited by every other role. SENTINEL can read checks but cannot push implementation code. VESSEL can inspect CI and perform delivery work, but it is not the author or reviewer of the application change.

The GitHub App's private key remains in the trusted control plane. Workspaces receive short-lived, repository-scoped installation tokens. The broker records the permission-profile hash and credential lease, while agents never receive the App's long-lived signing authority.

This creates two separate security boundaries:

1. The GitHub App installation defines the maximum permission ceiling accepted by the repository owner.
2. The OpenFlows broker narrows that ceiling for each agent role and purpose.

Configuring the App correctly is therefore necessary but insufficient. A broad App installation must not become a broad agent token.

## Independent review needs independent evidence

Separation of roles is weak if the reviewer merely trusts a test summary written by the builder.

In OpenFlows, SENTINEL sends verification commands to FORGE's isolated verification executor. The commands run against a temporary checkout of the candidate commit. The resulting evidence is associated with the review revision, round, and commit identity.

This preserves an important boundary:

- FORGE owns the build environment and implementation.
- SENTINEL chooses what evidence is required and decides whether that evidence is sufficient.
- The state machine records the decision and controls what can happen next.

The reviewer does not silently repair the builder's code and then approve its own repair. A rejection returns actionable findings to FORGE, which makes the change and submits a new candidate for another independent review.

The same rule continues into delivery. VESSEL does not treat “tests passed earlier” as merge evidence. It requires current-head CI, required approvals, and a mergeable pull request. Deployment is followed by health verification, and failure requires rollback rather than optimistic completion.

## A denied action is useful information

Least privilege occasionally blocks legitimate work. That is not evidence that the model is broken or that every agent needs more access.

It is evidence that the permission contract has discovered a real requirement.

When FORGE needed to update a GitHub Actions workflow, GitHub correctly rejected a token that had contents write but not workflows write. The right response was not to give every runtime a more powerful token. It was to identify the exact operation, add a typed workflow capability to the Forge Git profile, verify the App installation accepted that ceiling, and keep the permission absent from unrelated roles and API credentials.

This is how an agent permission system should evolve: from audited operations to explicit capabilities, with tests proving who receives the new power and who does not.

Permissions should be treated as versioned product behavior, not deployment trivia.

## The infrastructure is part of the agent

An autonomous agent is more than a model and a prompt. Its effective behavior includes:

- the credentials it can obtain;
- the files and networks it can access;
- the operations its hooks allow;
- the state transitions it can perform;
- the evidence required to advance;
- the independent actors that can reject its work;
- and the production actions it is structurally unable to take.

If a builder can delete production, approve the resulting change, merge it, and declare the deployment healthy, the problem is not simply that the agent made a bad decision. The system concentrated incompatible powers in one principal.

Good autonomous-agent infrastructure assumes that agents will sometimes misunderstand, overgeneralize, or choose the wrong action. It limits the blast radius of those mistakes and requires another role to validate consequential decisions.

The goal is not to make agents harmless by making them useless. The goal is to give each agent enough authority to perform its job—and no authority to certify or operate beyond that job.

Autonomy without boundaries is root access with better prose.

Autonomy with least privilege, separation of duties, independent evidence, and enforceable lifecycle gates is an engineering system we can reason about.
