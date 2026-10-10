ALTER TABLE outbox_events
    ADD COLUMN retry_at TIMESTAMPTZ,
    ADD COLUMN failed_at TIMESTAMPTZ;
