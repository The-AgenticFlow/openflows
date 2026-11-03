-- Runtime chat follow-ups are idempotent across Nexus restarts and poll retries.
-- Manager remains the sole holder of the Coder operator credential.
CREATE TABLE runtime_chat_messages (
    coder_chat_id TEXT NOT NULL REFERENCES runtime_chats(coder_chat_id) ON DELETE CASCADE,
    message_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (coder_chat_id, message_key)
);
