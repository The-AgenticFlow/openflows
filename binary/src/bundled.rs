use crate::orchestration::BundledFile;

pub(crate) fn bundled_files() -> Vec<BundledFile> {
    vec![
        // Agent configuration (schema v2)
        BundledFile {
            relative_path: "agent/registry.json",
            content: include_str!("../../orchestration/agent/registry.json"),
        },
        // Agent personas
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
        // Agent standards
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
        // Plugin MCP template
        BundledFile {
            relative_path: "plugin/mcp/mcp.json.template",
            content: include_str!("../../orchestration/plugin/mcp/mcp.json.template"),
        },
        // Skills — shared
        BundledFile {
            relative_path: "plugin/skills/shared-harness-protocol/SKILL.md",
            content: include_str!(
                "../../orchestration/plugin/skills/shared-harness-protocol/SKILL.md"
            ),
        },
        // Skills — forge
        BundledFile {
            relative_path: "plugin/skills/forge-coding/SKILL.md",
            content: include_str!("../../orchestration/plugin/skills/forge-coding/SKILL.md"),
        },
        BundledFile {
            relative_path: "plugin/skills/forge-planning/SKILL.md",
            content: include_str!("../../orchestration/plugin/skills/forge-planning/SKILL.md"),
        },
        // Skills — sentinel
        BundledFile {
            relative_path: "plugin/skills/sentinel-review/SKILL.md",
            content: include_str!("../../orchestration/plugin/skills/sentinel-review/SKILL.md"),
        },
        BundledFile {
            relative_path: "plugin/skills/sentinel-criteria/SKILL.md",
            content: include_str!("../../orchestration/plugin/skills/sentinel-criteria/SKILL.md"),
        },
        // Skills — vessel
        BundledFile {
            relative_path: "plugin/skills/vessel-merge-protocol/SKILL.md",
            content: include_str!(
                "../../orchestration/plugin/skills/vessel-merge-protocol/SKILL.md"
            ),
        },
        BundledFile {
            relative_path: "plugin/skills/vessel-ci-gate/SKILL.md",
            content: include_str!("../../orchestration/plugin/skills/vessel-ci-gate/SKILL.md"),
        },
        // Skills — lore
        BundledFile {
            relative_path: "plugin/skills/lore-documentation/SKILL.md",
            content: include_str!("../../orchestration/plugin/skills/lore-documentation/SKILL.md"),
        },
        BundledFile {
            relative_path: "plugin/skills/lore-changelog/SKILL.md",
            content: include_str!("../../orchestration/plugin/skills/lore-changelog/SKILL.md"),
        },
    ]
}
