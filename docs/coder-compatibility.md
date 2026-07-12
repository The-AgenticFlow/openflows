# Coder Compatibility

OpenFlows runs on top of a self-hosted Coder deployment. This document tracks
which Coder version OpenFlows has been verified against and any compatibility
considerations.

## Pinned Version

The Coder server image is pinned in `docker-compose.yml` via `CODER_IMAGE_TAG`.
The default is `latest` until a specific version has been verified against the
Chats API surface OpenFlows uses.

### Verification Checklist (Task 2.2)

Before relying on a Coder version in production, verify the following against the
live server at that version:

- [ ] **Chat creation**: `POST /api/experimental/chats` accepts a `workspace_id`
      to bind a chat to an existing workspace (control-plane agent, CLI mode is
      NOT used).
- [ ] **Model selection**: The `model_config_id` field in `CreateChatRequest`
      correctly references a model from `GET /api/experimental/chats/models`.
      If `model_config_id` is omitted, the server uses the default model.
- [ ] **Plan mode**: Chat creation supports a plan-mode flag (or equivalent)
      for review-only roles (SENTINEL, NEXUS).
- [ ] **Chat labels**: Labels (`ticket_id`, `role`, `flow`, `tenant`) are
      persisted and filterable via `GET /api/experimental/chats`.
- [ ] **WebSocket streaming**: `GET /api/experimental/chats/{chat}/stream`
      emits `status`, `message_part`, `error`, `action_required`, and
      `queue_update` events.
- [ ] **Headless external auth**: `coder external-auth access-token github`
      works from inside a workspace with a session token (no browser interaction
      required). This is critical for the Controller to poll GitHub issues and
      for worker workspaces to push code.
- [ ] **Workspace CRUD**: Standard `/api/v2/workspaces` endpoints for create,
      get, start, stop, delete work as expected.
- [ ] **Template push**: `POST /api/v2/templateversions` accepts `.tar.gz`
      archives with Terraform templates.

### Recording a Verified Version

Once verified, update `docker-compose.yml`:

```yaml
coder:
  image: ghcr.io/coder/coder:v2.X.Y
```

And record the version + verification date here:

| Version | Verified Date | Notes |
|---------|---------------|-------|
| latest  | pending       | Default; pin after live verification |

## Chats API Stability

The Coder Agents Chats API (`/api/experimental/chats`) is marked **experimental**
and may change between versions. OpenFlows isolates all Chats API calls in
`crates/coder-client`. If the API changes, only that crate needs updating.

## External Auth

GitHub external auth must be configured on the Coder server:
`CODER_EXTERNAL_AUTH_0_TYPE=github`, `CODER_EXTERNAL_AUTH_0_CLIENT_ID`,
`CODER_EXTERNAL_AUTH_0_CLIENT_SECRET`. This replaces all GitHub PAT usage —
agents authenticate via Coder's OAuth flow, not personal access tokens.
