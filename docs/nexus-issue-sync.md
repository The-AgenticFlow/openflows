# NEXUS Issue Sync Architecture

## The Problem: LLM-Dependent Ticket Discovery

When NEXUS runs in the orchestration loop, it needs to discover open GitHub issues and assign them to idle workers. The original design relied on the LLM to call `list_issues` via MCP tools during its turn. This caused three compounding failures:

### Failure 1: LLM ignores tool calls when context shows empty data

The store starts with `tickets: []` and `assignable_tickets: []`. The LLM sees these empty arrays in its context and interprets them as "the system has already checked and confirmed there are no tickets" — contradicting the persona instruction to "ALWAYS call `list_issues` first." The more specific context data overrides the general instruction.

**Symptom:** Nexus returns `no_work` on every turn without ever calling `list_issues`.

### Failure 2: MCP tool results never persist to the store

Even when the LLM does call `list_issues`, the result only enters the conversation history. No code in `NexusNode::post()` creates `Ticket` objects from the tool response. The `post` method only updates *existing* tickets:

```rust
// This finds nothing if tickets is []
if let Some(ticket) = tickets.iter_mut().find(|t| t.id == *ticket_id) {
    ticket.status = TicketStatus::Assigned { ... };
}
```

So after a `work_assigned` decision referencing a ticket discovered via MCP, the ticket is never written to the store. On the next loop iteration, the store still shows `tickets: []`.

### Failure 3: No escape hatch in the self-loop

The flow routes `no_work → nexus` with no mechanism to change state between iterations. No counter, no delay, no backoff. The loop runs until the `max_steps` safety cap (10,000 by default).

---

## The Solution: Programmatic Issue Sync

### Principle: Don't trust the LLM with state management

The LLM's job is **decision-making** (which ticket to assign, which worker gets it). State management (what tickets exist, what their status is) should be **programmatic and deterministic**. The LLM should never be the source of truth for what work exists — it should only reason over data that the system has already fetched and validated.

### Architecture: `sync_issues()` in `prep()`

```
NexusNode::prep()
    │
    ├── sync_registry()     ← existing: loads worker slots from registry.json
    ├── sync_issues()       ← NEW: fetches open GitHub issues via REST API
    │   ├── GET /repos/{owner}/{repo}/issues?state=open
    │   ├── Filter out pull requests (issue.pull_request field)
    │   ├── Upsert into store.tickets as Ticket { status: Open }
    │   └── Skip tickets already in store (idempotent)
    │
    ├── store.get_typed(KEY_TICKETS)     ← now populated with real issues
    ├── compute assignable_tickets       ← filtered from real data
    │
    └── Return context JSON to LLM       ← LLM sees populated tickets
```

The LLM now always sees current ticket data in its context. It no longer needs to call `list_issues` as a tool — the data is already there. The persona instruction to "call `list_issues`" becomes a fallback rather than a requirement.

### Why REST API instead of MCP

The `sync_issues()` method calls the GitHub REST API directly via `reqwest`, not through the MCP server. This is deliberate:

1. **Reliability:** MCP requires spawning a subprocess (`npx -y @modelcontextprotocol/server-github`), initializing a JSON-RPC session, and making a tool call through the protocol. REST is a single HTTP request.
2. **Speed:** No subprocess spawn overhead. The REST call completes in ~200ms vs ~1.5s for MCP initialization.
3. **Independence:** The MCP session is owned by `AgentRunner` and lifecycle-managed per `exec()` call. `sync_issues()` runs in `prep()`, which executes before `exec()`. Sharing the MCP session across phases would require architectural changes.
4. **Idempotency:** REST calls are stateless and easy to retry. MCP sessions have connection state that complicates retry logic.

### Ticket upsert logic

```rust
for issue in &gh_issues {
    // Skip PRs (GitHub returns PRs in the issues endpoint)
    if issue.pull_request.is_some() { continue; }

    let ticket_id = format!("T-{:03}", issue.number);

    // Skip if already tracked — preserves status updates from workers
    if tickets.iter().any(|t| t.id == ticket_id) { continue; }

    // Insert as Open — LLM will assign if worker is idle
    tickets.push(Ticket {
        id: ticket_id,
        title: issue.title.clone(),
        body: issue.body.clone().unwrap_or_default(),
        priority: 0,
        branch: None,
        status: TicketStatus::Open,
        issue_url: Some(issue.html_url.clone()),
        attempts: 0,
    });
}
```

Key design decisions:
- **Ticket ID format:** `T-XXX` where XXX is the GitHub issue number (zero-padded to 3 digits). Matches the persona's instruction format.
- **Skip existing:** If a ticket is already in the store (perhaps with `Assigned` or `Failed` status from a previous loop), we don't overwrite it. The store is the source of truth for ticket lifecycle.
- **Filter PRs:** GitHub's `/issues` endpoint returns both issues and PRs. The `pull_request` field distinguishes them.

---

## Ticket Creation in `post()`

When the LLM returns `work_assigned` with a `ticket_id` that doesn't exist in the store (e.g., it was discovered via MCP tool call in the same turn, or was created between sync cycles), `post()` now creates the ticket instead of silently dropping the assignment:

```rust
if let Some(ticket) = tickets.iter_mut().find(|t| t.id == *ticket_id) {
    // Update existing ticket
    ticket.status = TicketStatus::Assigned { worker_id: worker_id.clone() };
} else {
    // Create new ticket from the assignment
    tickets.push(Ticket {
        id: ticket_id.clone(),
        title: decision.notes.clone(),
        status: TicketStatus::Assigned { worker_id: worker_id.clone() },
        issue_url: decision.issue_url.clone(),
        ...
    });
}
```

This ensures no assignment is ever lost, regardless of whether the ticket was synced before or discovered during the LLM turn.

---

## No-Work Escape Hatch

A counter in the store (`_no_work_count`) tracks consecutive `no_work` decisions. After 3 consecutive `no_work` responses, NEXUS returns `STOP_SIGNAL` to halt the flow gracefully.

```rust
if decision.action == "no_work" {
    let count: u32 = store.get_typed(KEY_NO_WORK_COUNT).await.unwrap_or(0);
    let new_count = count + 1;
    store.set(KEY_NO_WORK_COUNT, json!(new_count)).await;

    if new_count >= NO_WORK_THRESHOLD {
        return Ok(Action::new(STOP_SIGNAL));
    }
}
```

The counter resets to 0 on any `work_assigned` decision:

```rust
if decision.action == "work_assigned" {
    store.set(KEY_NO_WORK_COUNT, json!(0)).await;
    ...
}
```

### Why threshold of 3

- **1 is too aggressive:** A single `no_work` after a forge failure (worker reset to idle, ticket marked failed) is normal — the next loop will see the ticket as assignable and reassign it.
- **2 catches rate-limiting:** If the GitHub API is temporarily rate-limited, `sync_issues()` fails gracefully and the LLM may return `no_work`. A second attempt gives the API time to recover.
- **3 is the right balance:** Three consecutive `no_work` decisions with programmatic issue sync means the system genuinely has no work to do. Continuing to loop wastes LLM tokens and API calls.

---

## Data Flow Diagram

```
                    ┌─────────────────────────┐
                    │     SharedStore          │
                    │                         │
                    │  tickets: []            │  ← initial state
                    │  worker_slots: {}       │
                    │  _no_work_count: 0      │
                    └────────┬────────────────┘
                             │
            ┌────────────────┼─────────────────┐
            │  NexusNode::prep()               │
            │                                  │
            │  1. sync_registry()              │
            │     → populate worker_slots      │
            │                                  │
            │  2. sync_issues()  ★ NEW         │
            │     → GET /repos/o/r/issues      │
            │     → upsert tickets[]           │
            │                                  │
            │  3. Build context JSON           │
            │     → tickets now populated      │
            │     → assignable_tickets real    │
            └────────────┬────────────────────┘
                         │
                         ▼
            ┌─────────────────────────┐
            │  NexusNode::exec()      │
            │                         │
            │  LLM sees real data     │
            │  → can assign work      │
            │  → no need to call      │
            │    list_issues tool     │
            └────────────┬────────────┘
                         │
                         ▼
            ┌─────────────────────────────┐
            │  NexusNode::post()           │
            │                              │
            │  if work_assigned:           │
            │    reset _no_work_count = 0  │
            │    update ticket status      │
            │    create ticket if needed ★ │
            │    update worker slot        │
            │                              │
            │  if no_work:                 │
            │    increment _no_work_count  │
            │    if >= 3: return STOP ★    │
            └────────────┬─────────────────┘
                         │
                    ┌────┴────┐
               work_assigned  no_work (count < 3)
                    │              │
                    ▼              ▼
              forge_pair       nexus (loop again)
                              (with fresh sync_issues)
```

---

## Impact on Token Usage

Before this fix, each nexus loop iteration spawned a new MCP session (~1.5s), made an LLM call (~2s), and burned tokens on a `no_work` decision. The logs show 9 wasted iterations (steps 0-8) before the LLM finally called `list_issues` on step 9 — consuming ~18 seconds and significant token budget.

After the fix:
- **Step 0:** `sync_issues()` populates tickets in ~200ms. LLM sees real data immediately and assigns work on the first turn.
- **No wasted MCP spawns** for issue discovery.
- **No wasted LLM turns** on empty context.

---

## Configuration

| Parameter | Default | Location | Description |
|-----------|---------|----------|-------------|
| `NO_WORK_THRESHOLD` | 3 | `agent-nexus/src/lib.rs` | Consecutive `no_work` before stopping |
| `GITHUB_PERSONAL_ACCESS_TOKEN` | (required) | `.env` | Used by `sync_issues()` for REST API auth |
| `GITHUB_REPOSITORY` | (required) | `.env` | `owner/repo` format, determines sync target |

---

## Testing

### Unit Testing sync_issues

The `sync_issues()` method can be tested with a mock HTTP server:

```rust
#[tokio::test]
async fn test_sync_issues_populates_store() {
    let store = SharedStore::new_in_memory();
    // Mock GitHub API returning two open issues
    // Assert tickets are upserted with correct IDs and status
}
```

### Integration Testing

Run `cargo run --bin real_test` and verify:
1. NEXUS assigns work on the **first** loop iteration (step 0)
2. No repeated `no_work` loops before ticket assignment
3. Flow stops after 3 consecutive `no_work` if no issues exist

---

## References

- [FORGE-SENTINEL Architecture](./forge-sentinel-arch.md)
- [FORGE-SENTINEL Pair Integration](./forge-pair-integration.md)
- [`crates/agent-nexus/src/lib.rs`](../crates/agent-nexus/src/lib.rs) — implementation
- [`orchestration/agent/agents/nexus.agent.md`](../orchestration/agent/agents/nexus.agent.md) — NEXUS persona
