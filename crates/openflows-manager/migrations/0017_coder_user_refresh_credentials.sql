-- A linked Coder identity must retain an encrypted refresh credential so the
-- Manager can perform user-owned Coder operations without copying a Coder
-- session into a tenant workspace. The value itself remains exclusively in
-- the configured secret provider.
ALTER TABLE coder_user_links
    ADD COLUMN coder_refresh_secret_ref TEXT;
