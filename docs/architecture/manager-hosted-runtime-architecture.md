# Manager-Mediated Runtime Architecture

## Purpose

This document explains the behaviour introduced when `openflows-manager` became the authority between OpenFlows runtimes and shared infrastructure. It does not repeat product onboarding or the general platform architecture. It focuses on runtime identity, token rotation, credential brokering, hook delivery, workspace control, and agent-to-agent (A2A) communication.

The broader system is described in [OpenFlows system architecture](openflows-system-architecture.md). This is the behavioural view of the hosted runtime.

## What changed with the Manager

In the hosted model, a workspace is a narrowly scoped runtime principal. It does not hold broad Coder or GitHub authority.

```mermaid
flowchart LR
    subgraph Runtime[Untrusted tenant runtime]
        N[Nexus controller]
        W[Forge / Sentinel / other worker]
    end
    subgraph Manager[Trusted OpenFlows control plane]
        RA[Runtime authentication]
        WC[Workspace control]
        GB[GitHub credential broker]
        HI[Coder hook ingress]
        OW[Durable operation workers]
    end
    subgraph Infra[Shared infrastructure]
        PG[(PostgreSQL)]
        SEC[Secret provider]
        C[Coder]
        GH[GitHub App API]
    end

    N -->|runtime access token| RA
    W -->|runtime access token| RA
    N -->|request worker| WC
    N -->|purpose api or git| GB
    W -->|purpose api or git| GB
    C -->|globally signed event| HI
    WC --> PG
    WC --> OW
    OW --> C
    RA --> PG
    GB --> PG
    GB --> SEC
    GB --> GH
    HI --> PG
    HI --> SEC
    N -. never receives Coder operator credential .-> C
    N -. never receives GitHub App private key .-> GH
```

The Manager is not a proxy for every byte exchanged by agents. It is the authority for identity, scope, credentials, provisioning, and lifecycle ingress. Tenant-local coordination remains in the tenant runtime plane.

## Complete runtime sequence

The complete flow has three consecutive phases. They are separated below so
the diagrams remain readable and render correctly in narrow documentation
previews.

### Phase 1: Nexus bootstrap and repository access

```mermaid
sequenceDiagram
    participant PW as Manager provision worker
    participant C as Coder
    participant N as Nexus
    participant M as Manager
    participant PG as PostgreSQL
    participant GH as GitHub
    participant R as Tenant Redis

    PW->>PG: Reserve Nexus and create one-time bootstrap
    PW->>C: Create workspace with mounted runtime bundle
    C->>N: Start controller
    N->>M: POST /runtime/register with bootstrap
    M->>PG: Consume bootstrap and mint runtime session
    M-->>N: Access token and rotating refresh token
    N->>M: Heartbeat with access token
    N->>M: Request GitHub token with purpose api
    M->>PG: Resolve workspace, tenant, repository and installation
    M->>GH: Exchange App JWT for repository-scoped token
    GH-->>M: Short-lived installation token
    M-->>N: Scoped GitHub credential
    N->>GH: Synchronize issues, pull requests and CI
    N->>R: Persist orchestration state and A2A pair-token hash
```

### Phase 2: Manager-mediated worker creation

```mermaid
sequenceDiagram
    participant N as Nexus
    participant R as Tenant Redis
    participant M as Manager
    participant PG as PostgreSQL
    participant PW as Manager provision worker
    participant C as Coder
    participant W as Worker workspace

    N->>R: Store pair-token hash and idempotency key
    N->>M: Request Forge or Sentinel with pair token and idempotency key
    M->>PG: Authorize Nexus, reserve capacity, create operation
    M-->>N: Workspace ID and operation ID
    PW->>PG: Claim operation lease and resolve pinned template
    PW->>C: Create pinned worker with scoped runtime bundle
    C->>W: Start workspace
    W->>M: Register with one-time bootstrap
    M-->>W: Worker runtime session
    W->>M: Heartbeat
    PW->>PG: Mark workspace running and complete operation
    N->>M: Poll worker status
    M-->>N: Running
```

### Phase 3: Forge and Sentinel A2A verification

```mermaid
sequenceDiagram
    participant S as Sentinel
    participant N as Nexus A2A relay
    participant R as Tenant Redis
    participant F as Forge

    S->>N: A2A verification request and pair token
    N->>R: Verify stored pair-token hash
    N->>R: Persist task and audit event
    F->>N: A2A claim and pair token
    F->>F: Execute verification
    F->>N: Progress and completion
    N->>R: Persist result and evidence
    S->>N: Read terminal A2A result
```

There are three distinct authorities:

1. **Manager authority** controls runtime identity, credentials, worker creation, and scope.
2. **Nexus authority** controls tenant orchestration, ticket assignment, A2A pairing, and agent coordination.
3. **Coder authority** creates and runs the execution environments requested by the Manager.

## Runtime identity and tokens

### Initial runtime bundle

The Manager prepares a workspace-specific `runtime.json` and mounts it into the Coder workspace. It contains the immutable workspace and tenant IDs, Manager URL, one-time bootstrap token, tenant Redis URL and namespace, and tenant hook key and URL.

It does not contain a GitHub App private key, Coder operator token, Manager database credential, or reusable platform credential.

```mermaid
flowchart LR
    SP[Secret provider] -->|read-only runtime.json| WS[Workspace startup]
    WS -->|one-time bootstrap| REG[Manager register endpoint]
    REG -->|hash lookup and atomic consume| PG[(PostgreSQL)]
    REG -->|access and refresh| PROC[Runtime process memory]
    PROC -->|Bearer access| API[Manager runtime endpoints]
    PROC -->|Bearer refresh after 401| REF[Manager refresh endpoint]
    REF -->|rotated access and refresh| PROC
```

| Credential | Purpose | Storage and lifetime | Reuse |
|---|---|---|---|
| Bootstrap token | Establish the first session for one workspace | Raw value in mounted bundle; hash in PostgreSQL; short-lived | Single use |
| Access token | Authenticate normal runtime API calls | Process memory; hash in PostgreSQL; short-lived | Reused until rejected |
| Refresh token | Rotate the runtime session | Process memory; hash in PostgreSQL; rotating family with a 24-hour sliding expiry after each successful refresh | Each generation is single use; an inactive family expires after 24 hours |

`RuntimeControlClient` serializes refresh under one mutex. If heartbeat and broker calls both receive `401`, only one consumes the refresh token. The other observes the new access token and retries. A request is retried only once.

### Server-derived scope

The client does not choose its authority. The access-token hash resolves this chain:

```text
runtime credential
  -> workspace and role
  -> tenant
  -> OpenFlows organization
  -> GitHub connection generation
  -> repository
  -> Coder owner and pinned release
```

Every privileged runtime action checks this chain. Changes to workspace, tenant, connection, or credential generations invalidate stale authority.

### Two kinds of heartbeat

The controller sends a Manager heartbeat every 30 seconds; worker runtimes also heartbeat. This updates control-plane liveness in PostgreSQL. Agent coordination heartbeats in tenant Redis are separate: Manager heartbeat answers whether the managed runtime is authenticated and alive, while Redis heartbeat helps Nexus determine whether an assigned agent is progressing.

## Credential broker

A runtime asks only for purpose `git` or `api`, never for arbitrary repository IDs or permissions.

```mermaid
sequenceDiagram
    participant W as Runtime
    participant B as Broker
    participant PG as PostgreSQL
    participant S as Secrets
    participant GH as GitHub

    W->>B: Request credential for git or api
    B->>PG: Resolve runtime scope
    PG-->>B: Workspace role tenant repository installation
    B->>B: Select permission profile
    B->>S: Read application signing key
    S-->>B: Signing key
    B->>GH: Request repository installation token
    GH-->>B: Token permissions and expiry
    B->>B: Validate returned scope
    B->>PG: Record credential lease and audit
    B-->>W: Return short lived repository token
```

The broker provides:

- **secret containment:** only Manager reads the App private key;
- **server-derived scope:** role and purpose select permissions;
- **short exposure:** workspaces receive only expiring installation tokens;
- **revocation evidence:** the lease records a fingerprint, expiry and generation values.

Nexus uses `api` to synchronize issues, pull requests, and CI. Worker startup uses `git` for clone/fetch/push and configures `gh` with the installation identity. Hosted commits therefore use the GitHub App bot rather than the signed-in human's OAuth identity.

## Manager-mediated worker control

Nexus decides that a worker is needed; Manager decides whether it is authorized and how it is provisioned.

```mermaid
sequenceDiagram
    autonumber
    participant N as Nexus
    participant R as Tenant Redis
    participant M as Manager runtime API
    participant PG as PostgreSQL
    participant W as Durable tenant worker
    participant C as Coder

    N->>N: Select role, slot, ticket and branch
    N->>N: Derive ticket pair token
    N->>R: Store pair-token hash and idempotency key
    N->>M: POST /runtime/workspaces
    Note over N,M: role, slot, ticket, branch and raw pair token
    M->>PG: Confirm caller is this tenant's Nexus
    M->>PG: Check role, quota, capacity and idempotency
    M->>PG: Reserve workspace and durable operation
    M-->>N: Workspace ID, operation ID and queued status
    W->>PG: Claim operation lease
    W->>PG: Resolve creator-owned Coder identity and pinned release
    W->>PG: Create bootstrap and runtime bundle
    W->>C: Create or start exact workspace
    C-->>W: Workspace state
    W->>PG: Persist state and complete operation
    N->>M: GET /runtime/workspaces/{id}
    M-->>N: Desired and observed state
```

The pair token is delivered into the worker definition for ticket-specific A2A authentication. It is not a Manager token. The stable idempotency key prevents duplicate workspaces. Database operation leases let another Manager replica reclaim work after a process or node crash.

## The two hook systems

The term “hooks” covers two different systems.

### Coder lifecycle HTTP hooks

Coder lifecycle/chat events cross a trust boundary and enter through Manager.

```mermaid
sequenceDiagram
    autonumber
    participant C as Coder lifecycle sender
    participant M as Manager hook ingress
    participant PG as PostgreSQL
    participant S as Secret provider
    participant H as Nexus hook server
    participant R as Tenant Redis
    participant F as Nexus flow loop

    C->>M: POST /api/v1/hooks/coder/chat and global JWT
    M->>M: Verify signature, audience, expiry, JTI and body hash
    M->>PG: Resolve workspace or owner to one tenant and Nexus
    M->>S: Read tenant hook key
    M->>M: Re-sign unchanged body for tenant Nexus audience
    M->>H: POST /experimental/hooks/chat and tenant JWT
    H->>H: Verify tenant signature and payload binding
    H->>R: Persist tenant hook audit
    H->>R: Publish reactive kick when applicable
    H-->>M: Hook decision
    M-->>C: Forward response
    R-->>F: Wake orchestration loop
```

The global Coder signature proves the event source. Manager then resolves the workspace to the tenant and converts that trust into a tenant-specific signature. Coder cannot select a tenant merely by URL, and one tenant cannot forge another tenant's event.

### Agent CLI hooks inside workspaces

These are shell scripts used by agent clients. They are distributed artifacts, not Coder HTTP callbacks.

```mermaid
flowchart LR
    BIN[Hooks bundled in OpenFlows binary] --> PUB[Nexus publishes role directories]
    PUB --> ART[(Tenant shared artifacts volume)]
    ART --> FS[Forge copies Forge hooks]
    ART --> SS[Sentinel copies Sentinel hooks]
    FS --> CFG1[Local agent settings]
    SS --> CFG2[Local agent settings]
    CFG1 --> HAR1[Local openflows-harness]
    CFG2 --> HAR2[Local openflows-harness]
    HAR1 --> A2A[Nexus A2A relay]
    HAR2 --> A2A
```

Nexus publishes role-specific hook directories before workers are dispatched. Startup copies only the current role's hooks and generates settings from scripts that exist. These hooks coordinate the local agent and harness; they grant no infrastructure authority and cannot bypass Manager runtime authorization.

## A2A behaviour

A2A is the tenant-local verification protocol between Sentinel and Forge, hosted by Nexus. Manager establishes and provisions the participants but is not on the A2A hot path.

```mermaid
sequenceDiagram
    autonumber
    participant N as Nexus scheduler and relay
    participant R as Tenant Redis
    participant S as Sentinel harness
    participant F as Forge harness

    N->>N: Derive pair token for ticket
    N->>R: Store only pair-token hash under pair ID
    Note over N,F: Manager supplies raw token in scoped workspace parameters
    S->>N: message/send with VerifyRequest, pair ID and token
    N->>R: Verify hash, create pending task and append audit
    F->>N: tasks/claim with role Forge, checkout version 3 and token
    N->>R: Transition pending to running
    F->>N: tasks/push_progress
    N->>R: Persist progress and audit
    F->>N: tasks/complete with VerifyResult
    N->>R: Persist completion and result
    S->>N: tasks/get or tasks/resubscribe
    N-->>S: Terminal result and evidence
```

- The pair ID identifies the ticket relationship; the pair token proves participation.
- Runtime and pair tokens are not interchangeable.
- Only Forge may claim the verification task, with the expected checkout protocol version.
- Tasks, results, and audit entries are mirrored to Redis so relay state survives Nexus restart.
- Redis ACL credentials and an immutable namespace prevent cross-tenant A2A access.

## Who talks to whom

| Caller | Callee | Authentication | Data | Purpose |
|---|---|---|---|---|
| Workspace startup | Manager register | One-time bootstrap | Bootstrap token | Establish runtime session |
| Nexus or worker | Manager runtime API | Access/refresh family | Heartbeat, worker commands, credential purpose | Privileged operations with derived scope |
| Manager provision worker | Coder API | Server-held operator credential | Template and workspace operations | Keep Coder authority out of workspaces |
| Manager broker | GitHub App API | Server-held App key/JWT | Exact repository and permissions | Mint short-lived credentials |
| Nexus or worker | GitHub | Installation token | Git and API operations | Repository work without personal tokens |
| Coder | Manager hook ingress | Global hook JWT | Lifecycle event | Authenticate source and resolve tenant |
| Manager hook ingress | Nexus hook server | Tenant hook JWT | Unchanged lifecycle event | Tenant delivery and policy response |
| Runtimes | Tenant Redis | Tenant Redis ACL and namespace | Orchestration, heartbeats, A2A and hook state | Tenant-local durable coordination |
| Sentinel/Forge | Nexus A2A relay | Ticket pair token | Tasks, progress and results | Agent verification protocol |
| Nexus publisher | Shared artifacts | Filesystem boundary | Hooks, personas and skills | Deterministic worker assets |

## State and secret ownership

| System | Authoritative data |
|---|---|
| PostgreSQL | Ownership, workspace roles, tenant/repository scope, operations and leases, credential hashes, GitHub leases, generations |
| Secret provider | Raw bootstraps, Redis passwords, hook keys, GitHub App key, workspace bundles |
| Redis | Tenant orchestration, agent heartbeats, worker slots, A2A hashes/tasks/results/audit, hook audit and reactive kicks |
| Coder | Workspace/build/agent execution state and runtime bundle mounting |
| Workspace memory | Current access/refresh pair and short-lived GitHub token |
| Shared artifacts | Published role hooks and deterministic runtime assets, never authorization state |

## Failure and recovery

| Failure | Behaviour |
|---|---|
| Access token expires | First `401` causes one serialized refresh and one retry |
| Refresh family expires | Runtime requires a new bootstrap/session |
| Manager worker dies during provisioning | Database lease expires; another replica reclaims the operation |
| Nexus repeats a worker request | Idempotency key returns/reuses the existing operation and workspace |
| Connection or workspace generation changes | Stale runtime scope or broker lease is rejected |
| Hook workspace cannot be resolved | Manager rejects the event instead of guessing a tenant |
| Nexus hook server is unavailable | Manager reports relay failure; it does not reroute the event |
| Nexus restarts | Orchestration and A2A state are recovered from tenant Redis |
| A2A token does not match | Relay rejects the method before mutating task state |
| Worker stops heartbeating | Nexus marks assignment unhealthy; Manager retains durable workspace state |

## Current boundaries

- Manager runtime credentials are not Redis passwords, A2A tokens, Coder tokens, or GitHub credentials.
- Manager is on workspace provisioning and lifecycle-hook paths, but not between each Forge/Sentinel A2A message.
- Broker single-flight coalescing is currently process-local; distributed single-flight remains an infrastructure improvement.
- Manager must reach the Nexus hook server, and workers must reach the Nexus A2A relay.
- Template releases are immutable; runtime and hook changes require a new release and workspace update/rebuild.

## Code map

| Behaviour | Implementation |
|---|---|
| Runtime routes | `crates/openflows-manager/src/routes/runtime.rs` |
| Credential issuance and scope | `crates/openflows-manager/src/runtime/credentials.rs` |
| Runtime bundles and tenant credentials | `crates/openflows-manager/src/runtime/tenant.rs` |
| GitHub broker | `crates/openflows-manager/src/runtime/broker.rs` |
| Managed worker authorization | `crates/openflows-manager/src/runtime/workspaces.rs` |
| Durable workspace provisioning | `crates/openflows-manager/src/provisioning/tenant_worker.rs` |
| Coder hook ingress and relay | `crates/openflows-manager/src/routes/coder_hooks.rs` |
| Nexus Manager client | `crates/agent-nexus/src/runtime_control.rs` |
| Nexus scheduling | `crates/agent-nexus/src/lib.rs` |
| A2A protocol | `crates/agent-nexus/src/a2a/http_server.rs`, `crates/agent-nexus/src/a2a/routing.rs` |
| Nexus hook consumer | `crates/agent-nexus/src/hooks/server.rs` |
| Controller composition | `binary/src/bin/agentflow.rs` |
| Workspace startup | `crates/coder-client/templates/openflows-*/main.tf` |
