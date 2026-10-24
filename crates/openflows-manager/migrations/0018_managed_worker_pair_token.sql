-- Carry the A2A pair token for managed (hosted) worker workspaces.
--
-- The tenant's Nexus controller generates a per-ticket A2A pair token and
-- records its hash in Redis. The worker workspace must receive the matching
-- plaintext token so its harness can authenticate pair-scoped verification
-- against the relay. Store the token on the workspace row so the managed
-- provisioner can forward it to the Coder template as a workspace parameter.

ALTER TABLE workspaces
    ADD COLUMN a2a_pair_token TEXT;
