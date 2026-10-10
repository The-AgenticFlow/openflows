# Transactions, Leases, and Idempotency: Making Background Operations Safe to Retry

When a background operation fails halfway through, retrying it is not automatically safe.

Imagine that a user asks an application to create a service instance. The API accepts the request and places a provisioning operation in a database queue. A **background worker** is a separate long-running process that periodically polls that queue, claims the next operation, and performs the slow work independently of the user's HTTP request.

For example, the worker might call a cloud provider's `CreateService` API, wait for the service to become ready, and save the result. If the worker crashes before saving the provider's response, the database still says `provisioning`, but the service may already exist.

If the system retries blindly, it may create a duplicate. If it never retries, the operation may remain stuck forever.

Reliable systems use several mechanisms together. Each one answers a different question:

**Transaction:** Which database changes must commit or roll back together?

**Operation lease:** Which execution currently has permission to advance this operation?

**Idempotency:** What should happen when the same logical request is repeated?

**Database constraint:** Which invalid states must the database reject?

## Start with the actors

The user requests an action. The API server authenticates the request and records durable work. The worker performs the slow background action. The external system creates the service.

The API does not keep the user's request open while the external operation runs. It records the work and returns an operation ID. The user can close the browser while the worker continues.

When the service is ready, the user asks the API for the operation status. The API reads the result from the database.

> **Diagram to insert:** Background operation lifecycle — user → API → database queue → worker → Coder or cloud API.

## What a transaction protects

Suppose the API must create a service record and schedule its provisioning. Writing those records separately creates a failure window: the service may exist without durable work scheduled to create it.

```sql
BEGIN;

INSERT INTO services (...);

INSERT INTO operations (..., state)
VALUES (..., 'queued');

COMMIT;
```

The transaction guarantees that these local database changes commit together or roll back together.

It does not include the cloud provider's API call. Keeping a database transaction open while waiting for a remote service would hold locks and still would not roll back the remote side effect if the process dies.

Transactions provide local atomicity. They do not provide atomicity across unrelated systems.

## What idempotency protects

Imagine that the client submits a request, but the network fails before it receives the response. The client retries.

Without an idempotency key, the server may create a second service. With one, both attempts identify the same logical request.

The server follows this contract:

**New key:** Accept the operation.

**Same key and same input:** Return the original result.

**Same key and different input:** Reject the conflicting reuse.

The server stores the key, a fingerprint of the request, and the result reference in the same transaction that accepts the operation. For an asynchronous API, the result can be the original operation and resource IDs.

> **Diagram to insert:** Idempotency retry flow — the response is lost, the client retries with the same key, and the API returns the original operation reference.

## What a database constraint protects

A database constraint is a rule that PostgreSQL checks for every write, including writes made by requests that arrive at the same time. It is the final guard against invalid state.

Suppose the product allows one GitHub installation connection per Openflows organization:

```sql
CREATE UNIQUE INDEX one_installation_per_org
    ON github_installations (organization_id);
```

Two concurrent requests can both ask whether organization A already has an installation and both receive “no.” They then both try to insert. The unique index allows one insert and rejects the other, so the database still contains one connection rather than two.

A foreign-key constraint can require every membership to refer to an existing organization. A check constraint can reject an invalid status. These rules are enforced even if there is a bug in the API or two requests race.

The application must translate the constraint result into a useful response. A uniqueness error alone does not tell the API whether the caller repeated the same request, whether another caller won the race, or whether the request conflicts with an existing record.

Idempotency uses the same idea. A unique index can enforce one record per scoped key, but the application must also store and compare the request fingerprint. A matching retry returns the original result; reusing the key with different input returns a conflict.

## What a lease protects

A durable operation survives the process that created it. Several workers may poll the operations table, so the system needs temporary ownership.

A worker claims an available operation by atomically recording:

```text
lease_owner
lease_token
lease_expires_at
```

The claim transaction locks an eligible row, assigns a fresh token, sets an expiry, and commits. PostgreSQL's `FOR UPDATE SKIP LOCKED` lets competing workers skip rows currently being claimed.

The row lock coordinates the short claim transaction. The persisted lease records ownership after that transaction ends.

A 30-second lease with a heartbeat every 10 seconds is one possible design. If the worker crashes, the lease eventually expires and another worker can reclaim the operation. A rejected heartbeat means the worker has lost authority and must stop making progress updates.

## The stale-worker problem

Lease expiry creates a subtle race: the old worker may still be alive after its lease expires. When it resumes, it might try to write progress after another worker has taken over.

The sequence is:

1. Worker A claims the operation with token A.
2. Worker A pauses.
3. The lease expires.
4. Worker B claims the operation with token B.
5. Worker A resumes and tries to complete the operation.
6. The database rejects Worker A's stale token.
7. Worker B completes the operation.

Every claim receives a fresh token. Every heartbeat, progress update, completion, and failure update checks the current owner, token, and unexpired lease.

```sql
UPDATE operations
SET state = 'succeeded',
    lease_owner = NULL,
    lease_token = NULL,
    lease_expires_at = NULL
WHERE id = $1
  AND lease_owner = $2
  AND lease_token = $3
  AND lease_expires_at > clock_timestamp();
```

The caller must treat zero affected rows as a rejected stale update.

The token protects database state. It does not physically stop the old process or cancel a request already sent to an external API.

> **Diagram to insert:** Stale-worker takeover — Worker A loses its lease, Worker B claims the operation, and A's later update is rejected.

## Recovering after an external call

Consider this sequence:

```text
Worker calls Coder API
Coder creates the resource
Worker crashes before recording success in PostgreSQL
```

The database cannot know that Coder completed the request. A replacement worker therefore needs a reconciliation mechanism in the Coder adapter.

1. Look up the resource using a stable identifier such as `operation-123`.
2. If it exists and matches the operation, record its ID and continue.
3. If it does not exist, create it.
4. If a conflicting resource exists, stop and report an operator-visible error.

If the provider supports idempotency keys, send the same stable action identity on every retry. The lease token changes for each worker claim, but the external action identity remains the same for the same intended side effect.

This pattern applies to every external integration. GitHub, Coder, and cloud providers need lookup by stable identity, provider idempotency, or reconciliation logic. The operation handler should not blindly repeat external calls.

## Putting the mechanisms together

1. The client sends a request with an idempotency key.
2. A transaction creates the resource record, operation, and any outbox event together.
3. A worker claims the operation with a temporary lease.
4. The worker performs the external action outside the database transaction.
5. The worker renews its lease while it is still the current owner.
6. The external step uses provider idempotency or reconciliation where possible.
7. The worker records progress only with its current claim token.
8. If it crashes, another worker reclaims the expired operation and resumes from the last durable checkpoint.

> **Diagram to insert:** Combined workflow — idempotent request, transaction, lease, external side effect, reconciliation, and guarded completion.

No single mechanism provides exactly-once execution across a database and an external service. The design instead makes each boundary explicit and makes retries converge safely.

## How to test the design

Test the failure scenarios directly:

1. Race several workers against one queued operation and verify that only one active claim is returned.
2. Expire a lease, reclaim the operation, and verify that the old token cannot heartbeat or complete it.
3. Reuse a worker name with a new token and verify that the old guard remains invalid.
4. Submit the same idempotent request concurrently and verify that it creates one logical operation.
5. Reuse an idempotency key with different input and verify rejection.
6. Simulate external success followed by a worker crash and verify reconciliation or provider idempotency.
7. Run concurrent requests against uniqueness constraints and verify that invalid state cannot be stored.

Database integration tests establish local concurrency behavior. Provider contract tests establish what the external service guarantees. One cannot substitute for the other.

## Final distinction

**Transaction:** Which local changes belong together?

**Lease:** Which worker may act now?

**Idempotency:** What should happen when the same intent is repeated?

**Constraint:** Which states are impossible?

Once those questions are separated, crash recovery becomes a design problem with explicit boundaries instead of a collection of hopeful retries.

## Further reading

- [PostgreSQL SELECT and row-locking documentation](https://www.postgresql.org/docs/16/sql-select.html)
- [PostgreSQL constraints](https://www.postgresql.org/docs/16/ddl-constraints.html)
- [AWS Builders' Library: Making retries safe with idempotent APIs](https://aws.amazon.com/builders-library/making-retries-safe-with-idempotent-APIs/)
- [Martin Kleppmann: How to do distributed locking](https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html)
