pub const HEADER: &str = "◇ OpenFlows 0.1.0";
pub const TAGLINE: &str = "I'll orchestrate your AI agents while you focus on what matters.";

pub const LOGO: &str = r#"
┌────────────────────────────────────────────────────────────────────────────────┐
│  ██████╗ ██████╗ ███████╗███╗   ██╗███████╗██╗      ██████╗ ██╗    ██╗███████╗ │
│ ██╔═══██╗██╔══██╗██╔════╝████╗  ██║██╔════╝██║     ██╔═══██╗██║    ██║██╔════╝ │
│ ██║   ██║██████╔╝█████╗  ██╔██╗ ██║█████╗  ██║     ██║   ██║██║ █╗ ██║███████╗ │
│ ██║   ██║██╔═══╝ ██╔══╝  ██║╚██╗██║██╔══╝  ██║     ██║   ██║██║███╗██║╚════██║ │
│ ╚██████╔╝██║     ███████╗██║ ╚████║██║     ███████╗╚██████╔╝╚███╔███╔╝███████║ │
│  ╚═════╝ ╚═╝     ╚══════╝╚═╝  ╚═══╝╚═╝     ╚══════╝ ╚═════╝  ╚══╝╚══╝ ╚══════╝ │
└────────────────────────────────────────────────────────────────────────────────┘
"#;

pub const SETUP_HEADER: &str = "OpenFlows setup";
pub const SECURITY_HEADER: &str = "Security disclaimer";

pub fn version_string() -> String {
    let git_hash = std::env::var("GIT_HASH")
        .unwrap_or_else(|_| "dev".to_string());
    format!("{} ({})", HEADER, git_hash)
}

pub fn get_logo_lines() -> Vec<String> {
    LOGO.lines().map(|s| s.to_string()).collect()
}
