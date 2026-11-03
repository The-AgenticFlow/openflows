use crate::orchestration::BundledFile;

pub(crate) fn bundled_files() -> Vec<BundledFile> {
    vec![
        // Worker hooks travel with the binary, including packaged deployments.
        BundledFile {
            relative_path: "plugin/hooks/forge/post_write_lint.sh",
            content: include_str!("../../orchestration/plugin/hooks/forge/post_write_lint.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/forge/pre_bash_guard.sh",
            content: include_str!("../../orchestration/plugin/hooks/forge/pre_bash_guard.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/forge/pre_compact_handoff.sh",
            content: include_str!("../../orchestration/plugin/hooks/forge/pre_compact_handoff.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/forge/pre_write_check.sh",
            content: include_str!("../../orchestration/plugin/hooks/forge/pre_write_check.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/forge/session_start.sh",
            content: include_str!("../../orchestration/plugin/hooks/forge/session_start.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/forge/stop_require_artifact.sh",
            content: include_str!(
                "../../orchestration/plugin/hooks/forge/stop_require_artifact.sh"
            ),
        },
        BundledFile {
            relative_path: "plugin/hooks/forge/subagent_start.sh",
            content: include_str!("../../orchestration/plugin/hooks/forge/subagent_start.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/forge/subagent_stop.sh",
            content: include_str!("../../orchestration/plugin/hooks/forge/subagent_stop.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/lore/session-start.sh",
            content: include_str!("../../orchestration/plugin/hooks/lore/session-start.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/nexus/init-session.sh",
            content: include_str!("../../orchestration/plugin/hooks/nexus/init-session.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/nexus/log-decision.sh",
            content: include_str!("../../orchestration/plugin/hooks/nexus/log-decision.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/sentinel/post_write_validate.sh",
            content: include_str!(
                "../../orchestration/plugin/hooks/sentinel/post_write_validate.sh"
            ),
        },
        BundledFile {
            relative_path: "plugin/hooks/sentinel/pre_bash_readonly_guard.sh",
            content: include_str!(
                "../../orchestration/plugin/hooks/sentinel/pre_bash_readonly_guard.sh"
            ),
        },
        BundledFile {
            relative_path: "plugin/hooks/sentinel/session_start.sh",
            content: include_str!("../../orchestration/plugin/hooks/sentinel/session_start.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/sentinel/stop_require_eval.sh",
            content: include_str!("../../orchestration/plugin/hooks/sentinel/stop_require_eval.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/sentinel/subagent_start.sh",
            content: include_str!("../../orchestration/plugin/hooks/sentinel/subagent_start.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/sentinel/subagent_stop.sh",
            content: include_str!("../../orchestration/plugin/hooks/sentinel/subagent_stop.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/vessel/log-merge-status.sh",
            content: include_str!("../../orchestration/plugin/hooks/vessel/log-merge-status.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/vessel/session-start.sh",
            content: include_str!("../../orchestration/plugin/hooks/vessel/session-start.sh"),
        },
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
        // Slash commands used by hosted worker Chats. These must be embedded
        // because packaged deployments do not have the source checkout.
        BundledFile {
            relative_path: "plugin/commands/address_review.md",
            content: include_str!("../../orchestration/plugin/commands/address_review.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/assign.md",
            content: include_str!("../../orchestration/plugin/commands/assign.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/check-ci.md",
            content: include_str!("../../orchestration/plugin/commands/check-ci.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/ci_fix.md",
            content: include_str!("../../orchestration/plugin/commands/ci_fix.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/document-pr.md",
            content: include_str!("../../orchestration/plugin/commands/document-pr.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/gate-approve.md",
            content: include_str!("../../orchestration/plugin/commands/gate-approve.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/handoff.md",
            content: include_str!("../../orchestration/plugin/commands/handoff.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/merge.md",
            content: include_str!("../../orchestration/plugin/commands/merge.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/plan.md",
            content: include_str!("../../orchestration/plugin/commands/plan.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/review.md",
            content: include_str!("../../orchestration/plugin/commands/review.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/segment-done.md",
            content: include_str!("../../orchestration/plugin/commands/segment-done.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/status-check.md",
            content: include_str!("../../orchestration/plugin/commands/status-check.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/status.md",
            content: include_str!("../../orchestration/plugin/commands/status.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/update-changelog.md",
            content: include_str!("../../orchestration/plugin/commands/update-changelog.md"),
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
