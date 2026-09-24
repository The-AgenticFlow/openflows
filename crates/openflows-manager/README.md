# OpenFlows Manager

`openflows-manager` is the centralized management service for the OpenFlows ecosystem. It exposes a versioned HTTP API (`/api/v1`) providing:

- **Fleet runtime state** (`/api/v1/fleet`, `/api/v1/fleet/{tenant}`) — aggregated and per-tenant ticket counts, active worker slots, fine-grained workflow phases, pending PRs, heartbeats, and escalations.
- **Tenant lifecycle management** (`/api/v1/tenants`, `/api/v1/tenants/{tenant}`, `/api/v1/tenants/{tenant}/clean`) — create, list, inspect, clean, and remove tenant workspaces sharing the same Rust services used by the CLI.
- **Normalized Kanban API** (`/api/v1/tenants/{tenant}/tickets`, `/api/v1/tenants/{tenant}/tickets/{ticket}`) — single canonical stage resolution (`open`, `planning`, `building`, `testing`, `review`, `awaiting_human`, `done`, `failed`) and rich ticket detail views (PR, Sentinel reviews, planning gates, handoff, and deployment records).
- **API Hardening & Structured Errors** — machine-readable error responses with request correlation IDs (`x-request-id`) and strict tenant isolation.

## Documentation

Full API endpoint specifications, payload schemas, and example responses are documented in [docs/api-v1.md](./docs/api-v1.md).

## Running the Server

```bash
cargo run -p openflows-manager
```

Default bind address is `127.0.0.1:3002`, customizable with `OPENFLOWS_MANAGER_ADDR`.
