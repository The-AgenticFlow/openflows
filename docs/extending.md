# Extending OpenFlows

OpenFlows is designed for plug-and-play extension. The single extension point is
`orchestration/agent/registry.json` (schema v2). No Rust code changes are needed
to add skills, MCP servers, or models.

## Add a Skill

1. Create a directory under `orchestration/plugin/skills/`:
   ```
   orchestration/plugin/skills/my-new-skill/
   └── SKILL.md
   ```

2. Write the skill instructions in `SKILL.md`. The Coder Agent discovers skills
   in `.agents/skills/` and loads them via the `read_skill` tool.

3. List the skill in `registry.json` under the role's `skills` array:
   ```json
   {
     "id": "forge",
     "skills": ["forge-coding", "my-new-skill", "shared-harness-protocol"]
   }
   ```

4. Restart the Controller (or let NEXUS reload the registry on the next poll cycle).

The Provisioner materializes the skill's `SKILL.md` into the worker workspace's
`.agents/skills/<name>/` directory at workspace boot time.

## Add an MCP Server

**Option A: Per-role via registry.json**

Add the MCP server config to the role's `mcp` object:
```json
{
  "id": "forge",
  "mcp": {
    "my-server": {
      "command": "npx",
      "args": ["-y", "@my-org/my-mcp-server"]
    }
  }
}
```

The Provisioner writes this as `.mcp.json` in the workspace.

**Option B: Centrally via Coder dashboard**

Go to AI Settings → MCP Servers in the Coder dashboard. Register the server
with tool allow/deny lists and availability policies (mandatory / opt-out / opt-in).
These apply to all agent chats, not just OpenFlows.

Both options coexist — workspace `.mcp.json` and dashboard MCP servers are merged.

## Enable a New Model

1. Configure the model in the Coder dashboard: AI Settings → Coder Agents → Models.
   Add the provider (Anthropic, OpenAI, Google, etc.) and the specific model.

2. Reference it in `registry.json` via the `model` field:
   ```json
   {
     "id": "forge",
     "model": "claude-sonnet-4-5"
   }
   ```

3. The Controller matches the `model` hint against `GET /api/experimental/chats/models`
   when creating chats. If the model isn't configured in Coder, chat creation fails
   with a clear error listing available models.

## Add a New Role

1. Add an entry to `registry.json`:
   ```json
   {
     "id": "qa",
     "enabled": true,
     "model": "claude-haiku-4-5",
     "plan_mode": true,
     "max_instances": 1,
     "skills": ["qa-testing", "shared-harness-protocol"],
     "mcp": {}
   }
   ```

2. Create the role persona: `orchestration/agent/agents/qa.agent.md`

3. Create the workspace template: `crates/coder-client/templates/openflows-qa/`

4. Add flow graph routing in the Controller (binary/src/bin/agentflow.rs).

Adding a role is the one extension that does require code changes (flow graph
routing + template). Skills, MCP, and models are config-only.

## Future: CLI Backend Reintroduction

The current design uses control-plane Coder Agents exclusively. The seam between
the Controller and workers is narrow: (Chats API + SharedStore). If Coder's
built-in agent underperforms for a specific role, a CLI backend (e.g., Claude Code
installed in the workspace) could be reintroduced behind the same Chats API +
SharedStore interface without redesigning the flow graph.
