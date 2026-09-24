# OpenFlows Manager API Specification (v1)

OpenFlows Manager is the centralized control-plane HTTP service for the OpenFlows ecosystem. It provides the trusted backend for `openflows-console` and other automation clients, abstracting direct Redis access and standardizing multi-tenant fleet state, tenant lifecycle management, and normalized Kanban ticket workflows.

---

## General Conventions

### Base URL
- Local default: `http://127.0.0.1:3002` (configurable via `OPENFLOWS_MANAGER_ADDR`)
- Versioned endpoints: `/api/v1/...`
- Operational health probes: `/health`, `/ready`

### Request Correlation & Tracing
- All requests accept an optional `x-request-id` header.
- If omitted, the server automatically generates a unique correlation ID (`req_<16-alphanumeric>`).
- Every HTTP response includes the `x-request-id` header.

### Structured Error Envelope
All error responses (4xx and 5xx) strictly follow a consistent, machine-readable envelope:

```json
{
  "error": {
    "code": "tenant_not_found",
    "message": "Tenant 'acme' was not found",
    "request_id": "req_ab12cd34ef56gh78"
  }
}
```

#### Standard Error Codes
| HTTP Status | Error Code | Description |
| ----------- | ---------- | ----------- |
| 400 | `invalid_request` | Malformed body, missing required parameters, or invalid fleet size |
| 400 | `invalid_tenant_name` | Name contains characters not permitted in Redis namespace operations |
| 400 | `invalid_repository` | Repository does not follow `owner/repo` format |
| 404 | `tenant_not_found` | The requested tenant namespace does not exist |
| 404 | `ticket_not_found` | Ticket ID not found within the specified tenant |
| 409 | `tenant_already_exists` | Tenant name or repository binding already registered |
| 500 | `internal_error` | Server-side execution or IO failure |
| 503 | `store_error` | Backing Redis storage unavailable or connection refused |

---

## 1. Platform & Operational Probes

### `GET /health`
Liveness probe for orchestrators and load balancers. Returns 200 OK as long as the service process is running.

#### Response
```json
{
  "status": "ok"
}
```

### `GET /ready`
Readiness probe that checks dependency reachability (pinging the backing store within a 1s deadline).

#### Response (Success: 200 OK)
```json
{
  "status": "ready"
}
```

#### Response (Failure: 503 Service Unavailable)
```json
{
  "status": "unavailable"
}
```

### `GET /api/v1`
Version indicator and capability marker.

#### Response
```json
{
  "version": "v1"
}
```

---

## 2. Fleet API

### `GET /api/v1/fleet`
Returns an aggregated runtime overview across all tenants.

#### Response (200 OK)
```json
{
  "total_tenants": 2,
  "tenants": [
    {
      "tenant": "acme",
      "repository": "acme/web",
      "ticket_counts": {
        "total": 3,
        "open": 1,
        "assigned": 1,
        "in_progress": 0,
        "merged": 0,
        "failed": 0,
        "completed": 0,
        "exhausted": 0,
        "awaiting_human": 1
      },
      "tickets": [...],
      "phases": {
        "T-001": {
          "phase": "planning",
          "role": "forge",
          "ts": 1727160000000
        }
      },
      "worker_slots": {
        "forge-1": {
          "id": "forge-1",
          "status": {
            "type": "working",
            "ticket_id": "T-001"
          },
          "workspace_id": "ws-123"
        }
      },
      "pending_prs": [],
      "ci_readiness": "ready",
      "heartbeats": {},
      "escalations": {
        "awaiting_human_count": 1,
        "failed_count": 0,
        "tickets": [
          {
            "id": "T-003",
            "title": "Security review",
            "reason": "Human intervention requested",
            "attempts": 1,
            "escalation_type": "awaiting_human"
          }
        ]
      }
    }
  ],
  "summary": {
    "total_tickets": 3,
    "total_active_workers": 1,
    "total_pending_prs": 0,
    "total_escalations": 1
  }
}
```

### `GET /api/v1/fleet/{tenant}`
Returns runtime fleet state for a single tenant.

#### Response (200 OK)
Identical to a single entry from `tenants[]` above.

---

## 3. Tenant Lifecycle API

### `GET /api/v1/tenants`
Lists all registered tenants with their repository binding.

#### Response (200 OK)
```json
[
  {
    "name": "acme",
    "repository": "acme/web"
  }
]
```

### `POST /api/v1/tenants`
Registers and provisions a new tenant environment.

#### Request Body
```json
{
  "repo": "acme/web",
  "name": "acme",
  "fleet": 1
}
```
* `name` (optional): Defaults to the repo name or owner.
* `fleet` (optional): Default `1`. Number of FORGE-SENTINEL worker pairs.

#### Response (201 Created)
```json
{
  "tenant": "acme",
  "repository": "acme/web",
  "fleet": 1,
  "workspace_id": "ws-nexus-acme",
  "message": "Tenant created successfully"
}
```

### `GET /api/v1/tenants/{tenant}`
Fetches environment details for a specific tenant.

#### Response (200 OK)
```json
{
  "name": "acme",
  "repository": "acme/web",
  "registry": { ... },
  "key_count": 12,
  "ticket_count": 4,
  "active_workers": 1
}
```

### `POST /api/v1/tenants/{tenant}/clean`
Cleans runtime state: resets failed and awaiting_human tickets to open, resets attempts to 0, clears recovery counters, and resets worker slots.

#### Request Body (optional)
```json
{
  "reset_all": false
}
```

#### Response (200 OK)
```json
{
  "tenant": "acme",
  "reset_tickets_count": 2,
  "cleared_recovery_counters": 1,
  "message": "Tenant 'acme' cleaned successfully"
}
```

### `DELETE /api/v1/tenants/{tenant}?purge=true`
Removes tenant registration and optionally purges its entire Redis keyspace (`ns:{tenant}:*`).

#### Query Parameters
- `purge` (boolean, default: `true`): If `true`, deletes all Redis keys under `ns:{tenant}:*`.

#### Response (200 OK)
```json
{
  "tenant": "acme",
  "purged_keys_count": 12,
  "message": "Tenant 'acme' removed successfully"
}
```

---

## 4. Kanban & Normalized Ticket API

### `GET /api/v1/tenants/{tenant}/tickets`
Lists tickets with derived canonical Kanban stages.

#### Derived Kanban Stages
| Canonical Stage | Raw Status & Phase Condition |
| --------------- | ---------------------------- |
| `open` | `status: open` and no active workflow phase |
| `planning` | `status: open/assigned/in_progress` with `phase: planning` |
| `building` | `status: assigned/in_progress` with `phase: building` |
| `testing` | `status: assigned/in_progress` with `phase: testing` |
| `review` | `status: assigned/in_progress` with `phase: review_ready` |
| `awaiting_human`| `status: awaiting_human` or `phase: blocked` |
| `done` | `status: merged` or `status: completed` |
| `failed` | `status: failed` or `status: exhausted` |

#### Response (200 OK)
```json
[
  {
    "id": "T-001",
    "title": "Implement auth middleware",
    "body": "Add correlation ID tracking",
    "priority": 1,
    "branch": "feat/T-001-auth",
    "issue_url": "https://github.com/acme/web/issues/1",
    "attempts": 0,
    "raw_status": { "type": "assigned", "worker_id": "forge-1" },
    "status_type": "assigned",
    "stage": "planning",
    "stage_label": "Planning",
    "assigned_worker": "forge-1",
    "phase": {
      "phase": "planning",
      "role": "forge",
      "ts": 1727160000000
    }
  }
]
```

### `GET /api/v1/tenants/{tenant}/tickets/{ticket}`
Returns complete normalized ticket detail including PR, Sentinel review, planning gate, handoff, and deployment artifacts.

#### Response (200 OK)
```json
{
  "id": "T-001",
  "tenant": "acme",
  "title": "Implement auth middleware",
  "priority": 1,
  "raw_status": { "type": "assigned", "worker_id": "forge-1" },
  "status_type": "assigned",
  "stage": "review",
  "stage_label": "Review",
  "assigned_worker": "forge-1",
  "phase": { "phase": "review_ready", "role": "sentinel" },
  "pr": { "number": 42, "title": "Implement auth" },
  "pending_pr": null,
  "review": { "verdict": "approve", "report": "All tests pass" },
  "reviews": [
    { "role": "sentinel", "payload": { "verdict": "approve" } }
  ],
  "gate": { "approved_by": "sentinel", "ts": 1727160000000 },
  "gates": [
    { "phase": "planning", "payload": { "approved_by": "sentinel" } }
  ],
  "handoff": null,
  "deployment": null
}
```
