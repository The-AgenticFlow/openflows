pub mod agent;
pub mod identity;
pub mod registry;
pub mod state;

pub use agent::{AgentDef, AgentPermissions};
pub use identity::{AgentIdentity, AgentRole, IdentityManager};
pub use registry::{CliBackend, Registry, RegistryEntry, DEFAULT_CLI_ENV_VAR};
pub use state::*;

pub fn is_denylisted(path: &std::path::Path) -> bool {
    path.components().any(|component| {
        if let std::path::Component::Normal(os_str) = component {
            let s = os_str.to_string_lossy();
            s == ".codex"
                || s == ".claude"
                || s == ".agents"
                || s == ".pair-shared"
                || s == "worktrees"
                || s == "orchestration"
                || s == ".codex-home"
                || s.starts_with(".env")
        } else {
            false
        }
    })
}
