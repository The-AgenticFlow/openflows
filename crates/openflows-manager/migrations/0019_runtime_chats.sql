-- A managed chat belongs to one tenant worker assignment. Retain the Coder ID
-- so retries after a controller restart do not launch another agent.
CREATE TABLE runtime_chats (
    workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    ticket_id TEXT NOT NULL,
    coder_workspace_id TEXT NOT NULL,
    coder_chat_id TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, ticket_id)
);
