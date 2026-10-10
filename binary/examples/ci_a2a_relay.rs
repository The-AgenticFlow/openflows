//! Container test fixture hosting the production relay. Seeds authentication,
//! never lifecycle or verification outcomes. Not installed with OpenFlows.
use anyhow::{Context, Result};
use pocketflow_core::SharedStore;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let redis = std::env::var("REDIS_URL")?;
    let tenant = std::env::var("OPENFLOWS_TENANT")?;
    let token = std::env::var("A2A_PAIR_TOKEN").context("fixture requires pair token")?;
    let store = SharedStore::new_redis_with_tenant(&redis, Some(tenant)).await?;
    store
        .set(
            &agent_nexus::a2a::A2ARelay::pair_token_key("T-1"),
            json!(agent_nexus::a2a::A2ARelay::hash_pair_token(&token)),
        )
        .await;
    let _relay = agent_nexus::a2a::start_a2a_relay(Arc::new(store)).await?;
    std::future::pending::<()>().await;
    Ok(())
}
