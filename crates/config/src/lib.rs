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
