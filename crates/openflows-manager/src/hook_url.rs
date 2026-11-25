//! Where a tenant's Nexus receives Coder lifecycle hooks. The relay and the credential bundle
//! must derive the same value, because the controller checks it as the hook token's audience.

use crate::id::OrganizationId;

/// Hook destination for one tenant, from `OPENFLOWS_RUNTIME_RELAY_BASE_URL`.
///
/// `{tenant}` expands to the tenant id. `{namespace}` expands to the namespace of the tenant's
/// organization on the Kubernetes backend, so one base URL can name any tenant's Nexus Service.
pub fn runtime_hook_url(organization: uuid::Uuid, tenant: uuid::Uuid) -> Result<String, String> {
    hook_url_from_base(
        &std::env::var("OPENFLOWS_RUNTIME_RELAY_BASE_URL").unwrap_or_default(),
        tenant,
        &crate::runtime::delivery::organization_namespace(OrganizationId::from_uuid(organization)),
    )
}

/// Checks the relay base without a real tenant, for startup and `doctor`.
pub fn validate() -> Result<(), String> {
    runtime_hook_url(uuid::Uuid::nil(), uuid::Uuid::nil()).map(|_| ())
}

fn hook_url_from_base(base: &str, tenant: uuid::Uuid, namespace: &str) -> Result<String, String> {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("OPENFLOWS_RUNTIME_RELAY_BASE_URL is required for Coder hook relay".into());
    }
    let expanded = base
        .replace("{tenant}", &tenant.to_string())
        .replace("{namespace}", namespace);
    if expanded.contains(['{', '}']) {
        return Err("OPENFLOWS_RUNTIME_RELAY_BASE_URL has an unknown placeholder; use {tenant} or {namespace}".into());
    }
    let url = url::Url::parse(&expanded)
        .map_err(|_| "OPENFLOWS_RUNTIME_RELAY_BASE_URL must be an absolute HTTP(S) URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("OPENFLOWS_RUNTIME_RELAY_BASE_URL must be HTTP(S) without credentials, query, or fragment".into());
    }
    Ok(format!("{expanded}/experimental/hooks/chat"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: &str = "openflows-org";

    #[test]
    fn expands_tenant_and_normalizes_base() {
        let tenant = uuid::Uuid::nil();
        assert_eq!(
            hook_url_from_base(" http://127.0.0.1:7081/tenants/{tenant}/ ", tenant, NS).unwrap(),
            format!("http://127.0.0.1:7081/tenants/{tenant}/experimental/hooks/chat")
        );
    }

    #[test]
    fn expands_the_organization_namespace_into_a_service_address() {
        let tenant = uuid::Uuid::from_u128(7);
        assert_eq!(
            hook_url_from_base(
                "http://openflows-nexus-{tenant}.{namespace}.svc.cluster.local:3001",
                tenant,
                NS,
            )
            .unwrap(),
            format!(
                "http://openflows-nexus-{tenant}.openflows-org.svc.cluster.local:3001/experimental/hooks/chat"
            )
        );
    }

    #[test]
    fn a_misspelled_placeholder_is_rejected_instead_of_left_in_the_host() {
        let error = hook_url_from_base("http://nexus.{namepace}.svc:3001", uuid::Uuid::nil(), NS)
            .expect_err("rejected");
        assert!(error.contains("unknown placeholder"));
    }

    #[test]
    fn a_base_without_placeholders_is_used_as_is() {
        assert_eq!(
            hook_url_from_base("https://relay.example", uuid::Uuid::nil(), NS).unwrap(),
            "https://relay.example/experimental/hooks/chat"
        );
    }

    #[test]
    fn rejects_invalid_relay_urls() {
        for base in [
            "",
            "relative/path",
            "ftp://relay",
            "http://user:secret@relay",
            "http://relay?x=1",
            "http://relay#fragment",
        ] {
            assert!(hook_url_from_base(base, uuid::Uuid::nil(), NS).is_err());
        }
    }
}
