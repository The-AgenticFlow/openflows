-- Remember the GitHub login an invitation was created for, so admins can see
-- who a pending invitation is for before that person has ever signed in to
-- OpenFlows (until then there is no identity row to read a login from). The
-- immutable invitee_github_user_id stays the authority for acceptance; this
-- is display-only and may go stale if the user renames their account.
ALTER TABLE invitations ADD COLUMN invitee_login TEXT;
