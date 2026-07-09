use anyhow::Result;
use pocketflow_core::SharedStore;
use crate::state::{WorkerSlot, KEY_WORKER_SLOTS, KEY_TICKETS, Ticket};
use std::collections::HashMap;

pub async fn debug_system() -> Result<()> {
    println!("=== AgentFlow Debug Info ===");
    
    // Check Redis / Store
    let store = if let Ok(url) = std::env::var("REDIS_URL") {
        println!("Store: Redis ({})", url);
        SharedStore::new_redis(&url).await?
    } else {
        println!("Store: In-Memory (No persistence)");
        SharedStore::new_in_memory()
    };

    let slots: HashMap<String, WorkerSlot> = store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
    println!("\n--- Workers ---");
    if slots.is_empty() {
        println!("No workers found.");
    } else {
        for (id, slot) in slots {
            println!("Worker ID: {}", id);
            println!("  Status: {:?}", slot.status);
            println!("  Workspace ID: {:?}", slot.workspace_id);
            println!("  Assigned Ticket: {:?}", slot.assigned_ticket);
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
    for var in &["USE_AI_GATEWAY", "WORKSPACE_PROVIDER", "CODER_URL", "ANTHROPIC_MODEL", "OPENAI_MODEL"] {
        println!("{}: {:?}", var, std::env::var(var).unwrap_or_else(|_| "NOT SET".to_string()));
    }

    Ok(())
}
