-- Device delivery must preserve the browser's actual authentication time,
-- not make an old login fresh by issuing a new CLI credential.
ALTER TABLE cli_login_requests
    ADD COLUMN approved_authenticated_at TIMESTAMPTZ;
