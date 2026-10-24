-- Reserve key identity before calling Coder, so interrupted key creation can
-- revoke only the key owned by this deployment and organization.
CREATE TABLE managed_provisioners (
    coder_url TEXT NOT NULL,
    coder_organization_id UUID NOT NULL,
    key_name TEXT NOT NULL,
    sealed_key BYTEA,
    PRIMARY KEY (coder_url, coder_organization_id),
    UNIQUE (key_name)
);
