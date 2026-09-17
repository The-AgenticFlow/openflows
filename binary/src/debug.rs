use crate::state::{Ticket, WorkerSlot, KEY_TICKETS, KEY_WORKER_SLOTS};
use anyhow::Result;
use config::Envconfig;
use pocketflow_core::SharedStore;
use std::collections::HashMap;

/// Return the compiled binary version (from `CARGO_PKG_VERSION`).
pub fn binary_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Return the configured workspace provider, falling back to a readable marker
/// when unset so the debug dump always shows an explicit value.
pub fn workspace_provider() -> String {
    std::env::var("WORKSPACE_PROVIDER").unwrap_or_else(|_| "not-set".to_string())
}

pub async fn debug_system() -> Result<()> {
    println!("=== AgentFlow Debug Info ===");
    println!("OpenFlows version: {}", binary_version());

    // Check Redis / Store
    let store = if let Some(url) = config::InfraConfig::init_from_env()?.redis_url {
        println!("Store: Redis ({})", url);
        SharedStore::new_redis(&url).await?
    } else {
        println!("Store: In-Memory (No persistence)");
        SharedStore::new_in_memory()
    };

    let slots: HashMap<String, WorkerSlot> =
        store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
    println!("\n--- Workers ---");
    if slots.is_empty() {
        println!("No workers found.");
    } else {
        for (id, slot) in slots {
            println!("Worker ID: {}", id);
            println!("  Status: {:?}", slot.status);
            println!("  Workspace ID: {:?}", slot.workspace_id);
        }
    }

    let tickets: HashMap<String, Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
    println!("\n--- Tickets ---");
    if tickets.is_empty() {
        println!("No tickets found.");
    } else {
        for (id, ticket) in tickets {
            println!("Ticket ID: {}", id);
            println!("  Status: {:?}", ticket.status);
            println!("  Title: {}", ticket.title);
        }
    }

    println!("\n--- Environment ---");
    println!("WORKSPACE_PROVIDER: {}", workspace_provider());
    for var in &[
        "USE_AI_GATEWAY",
        "CODER_URL",
        "ANTHROPIC_MODEL",
        "OPENAI_MODEL",
    ] {
        println!(
            "{}: {:?}",
            var,
            std::env::var(var).unwrap_or_else(|_| "NOT SET".to_string())
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::workspace_provider;

    #[test]
    fn workspace_provider_reports_set_value() {
        unsafe { std::env::set_var("WORKSPACE_PROVIDER", "coder") };
        assert_eq!(workspace_provider(), "coder");
        unsafe { std::env::remove_var("WORKSPACE_PROVIDER") };
    }

    #[test]
    fn workspace_provider_falls_back_when_unset() {
        unsafe { std::env::remove_var("WORKSPACE_PROVIDER") };
        assert_eq!(workspace_provider(), "not-set");
    }
}
