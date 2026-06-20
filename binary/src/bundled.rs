use crate::orchestration::BundledFile;

/// Minimal set of files required for binary startup.
/// Hook scripts, skills, commands, and standards are distributed via
/// tarball/install and are not embedded to keep binary size small.
pub(crate) fn bundled_files() -> Vec<BundledFile> {
    vec![
        // Agent configuration (required)
        BundledFile {
            relative_path: "agent/registry.json",
            content: include_str!("../../orchestration/agent/registry.json"),
        },
        // Agent personas (required for startup)
        BundledFile {
            relative_path: "agent/agents/nexus.agent.md",
            content: include_str!("../../orchestration/agent/agents/nexus.agent.md"),
        },
        BundledFile {
            relative_path: "agent/agents/forge.agent.md",
            content: include_str!("../../orchestration/agent/agents/forge.agent.md"),
        },
        BundledFile {
            relative_path: "agent/agents/sentinel.agent.md",
            content: include_str!("../../orchestration/agent/agents/sentinel.agent.md"),
        },
        BundledFile {
            relative_path: "agent/agents/vessel.agent.md",
            content: include_str!("../../orchestration/agent/agents/vessel.agent.md"),
        },
        BundledFile {
            relative_path: "agent/agents/lore.agent.md",
            content: include_str!("../../orchestration/agent/agents/lore.agent.md"),
        },
        // Standards (small, useful to have embedded)
        BundledFile {
            relative_path: "agent/standards/CODING.md",
            content: include_str!("../../orchestration/agent/standards/CODING.md"),
        },
        BundledFile {
            relative_path: "agent/standards/REVIEW.md",
            content: include_str!("../../orchestration/agent/standards/REVIEW.md"),
        },
        BundledFile {
            relative_path: "agent/standards/SECURITY.md",
            content: include_str!("../../orchestration/agent/standards/SECURITY.md"),
        },
    ]
}
