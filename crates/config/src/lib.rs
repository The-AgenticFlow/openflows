pub mod agent;
pub mod identity;
pub mod project;
pub mod registry;
pub mod state;

pub use agent::{AgentDef, AgentPermissions};
pub use identity::{AgentIdentity, AgentRole, IdentityManager};
pub use project::{ProjectConfig, SandboxConfig, PROJECT_CONFIG_FILE};
pub use registry::{CliBackend, Registry, RegistryEntry, DEFAULT_CLI_ENV_VAR};
pub use state::*;

pub fn is_denylisted(path: &std::path::Path) -> bool {
    for (i, component) in path.components().enumerate() {
        if let std::path::Component::Normal(os_str) = component {
            let s = os_str.to_string_lossy();
            // Config/sync dirs: denylisted at any depth
            if s == ".codex" || s == ".claude" || s == ".agents" || s == ".pair-shared" {
                return true;
            }
            // Root-level orchestration metadata dirs (not user project dirs)
            if i == 0 && (s == "worktrees" || s == "orchestration" || s == ".codex-home") {
                return true;
            }
            // .env and .env.* files at any depth
            if s == ".env" || s.starts_with(".env.") {
                return true;
            }
        }
    }
    false
}
