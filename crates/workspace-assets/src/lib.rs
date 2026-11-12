//! The immutable workspace artifact bundle.
//!
//! These are the files every role workspace needs at boot: the role personas,
//! the skill and command trees, the standards, and the hooks. They are compiled
//! into whichever binary ships them, so the controller and the Manager can both
//! publish the exact bundle of the release that built them.
//!
//! The bundle is content-addressed. [`archive`] serializes the tree into a
//! deterministic gzipped tar and [`digest`] is the identifier a workspace pins,
//! which lets a workspace verify what it received and reject a stale copy.

use sha2::{Digest, Sha256};

/// One embedded artifact, at the path it takes inside the bundle.
pub struct BundledFile {
    pub relative_path: &'static str,
    pub content: &'static str,
}

/// Every artifact this release ships, in bundle order.
pub fn files() -> Vec<BundledFile> {
    vec![
        // Worker hooks travel with the binary, including packaged deployments.
        BundledFile {
            relative_path: "plugin/hooks/forge/post_write_lint.sh",
            content: include_str!("../../../orchestration/plugin/hooks/forge/post_write_lint.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/forge/pre_bash_guard.sh",
            content: include_str!("../../../orchestration/plugin/hooks/forge/pre_bash_guard.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/forge/pre_compact_handoff.sh",
            content: include_str!(
                "../../../orchestration/plugin/hooks/forge/pre_compact_handoff.sh"
            ),
        },
        BundledFile {
            relative_path: "plugin/hooks/forge/pre_write_check.sh",
            content: include_str!("../../../orchestration/plugin/hooks/forge/pre_write_check.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/forge/session_start.sh",
            content: include_str!("../../../orchestration/plugin/hooks/forge/session_start.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/forge/stop_require_artifact.sh",
            content: include_str!(
                "../../../orchestration/plugin/hooks/forge/stop_require_artifact.sh"
            ),
        },
        BundledFile {
            relative_path: "plugin/hooks/forge/subagent_start.sh",
            content: include_str!("../../../orchestration/plugin/hooks/forge/subagent_start.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/forge/subagent_stop.sh",
            content: include_str!("../../../orchestration/plugin/hooks/forge/subagent_stop.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/lore/session-start.sh",
            content: include_str!("../../../orchestration/plugin/hooks/lore/session-start.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/nexus/init-session.sh",
            content: include_str!("../../../orchestration/plugin/hooks/nexus/init-session.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/nexus/log-decision.sh",
            content: include_str!("../../../orchestration/plugin/hooks/nexus/log-decision.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/sentinel/post_write_validate.sh",
            content: include_str!(
                "../../../orchestration/plugin/hooks/sentinel/post_write_validate.sh"
            ),
        },
        BundledFile {
            relative_path: "plugin/hooks/sentinel/pre_bash_readonly_guard.sh",
            content: include_str!(
                "../../../orchestration/plugin/hooks/sentinel/pre_bash_readonly_guard.sh"
            ),
        },
        BundledFile {
            relative_path: "plugin/hooks/sentinel/session_start.sh",
            content: include_str!("../../../orchestration/plugin/hooks/sentinel/session_start.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/sentinel/stop_require_eval.sh",
            content: include_str!(
                "../../../orchestration/plugin/hooks/sentinel/stop_require_eval.sh"
            ),
        },
        BundledFile {
            relative_path: "plugin/hooks/sentinel/subagent_start.sh",
            content: include_str!("../../../orchestration/plugin/hooks/sentinel/subagent_start.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/sentinel/subagent_stop.sh",
            content: include_str!("../../../orchestration/plugin/hooks/sentinel/subagent_stop.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/vessel/log-merge-status.sh",
            content: include_str!("../../../orchestration/plugin/hooks/vessel/log-merge-status.sh"),
        },
        BundledFile {
            relative_path: "plugin/hooks/vessel/session-start.sh",
            content: include_str!("../../../orchestration/plugin/hooks/vessel/session-start.sh"),
        },
        // Agent configuration (schema v2)
        BundledFile {
            relative_path: "agent/registry.json",
            content: include_str!("../../../orchestration/agent/registry.json"),
        },
        // Agent personas
        BundledFile {
            relative_path: "agent/agents/nexus.agent.md",
            content: include_str!("../../../orchestration/agent/agents/nexus.agent.md"),
        },
        BundledFile {
            relative_path: "agent/agents/forge.agent.md",
            content: include_str!("../../../orchestration/agent/agents/forge.agent.md"),
        },
        BundledFile {
            relative_path: "agent/agents/sentinel.agent.md",
            content: include_str!("../../../orchestration/agent/agents/sentinel.agent.md"),
        },
        BundledFile {
            relative_path: "agent/agents/vessel.agent.md",
            content: include_str!("../../../orchestration/agent/agents/vessel.agent.md"),
        },
        BundledFile {
            relative_path: "agent/agents/lore.agent.md",
            content: include_str!("../../../orchestration/agent/agents/lore.agent.md"),
        },
        // Agent standards
        BundledFile {
            relative_path: "agent/standards/CODING.md",
            content: include_str!("../../../orchestration/agent/standards/CODING.md"),
        },
        BundledFile {
            relative_path: "agent/standards/REVIEW.md",
            content: include_str!("../../../orchestration/agent/standards/REVIEW.md"),
        },
        BundledFile {
            relative_path: "agent/standards/SECURITY.md",
            content: include_str!("../../../orchestration/agent/standards/SECURITY.md"),
        },
        // Plugin MCP template
        BundledFile {
            relative_path: "plugin/mcp/mcp.json.template",
            content: include_str!("../../../orchestration/plugin/mcp/mcp.json.template"),
        },
        // Slash commands used by hosted worker Chats. These must be embedded
        // because packaged deployments do not have the source checkout.
        BundledFile {
            relative_path: "plugin/commands/address_review.md",
            content: include_str!("../../../orchestration/plugin/commands/address_review.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/assign.md",
            content: include_str!("../../../orchestration/plugin/commands/assign.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/check-ci.md",
            content: include_str!("../../../orchestration/plugin/commands/check-ci.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/ci_fix.md",
            content: include_str!("../../../orchestration/plugin/commands/ci_fix.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/document-pr.md",
            content: include_str!("../../../orchestration/plugin/commands/document-pr.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/gate-approve.md",
            content: include_str!("../../../orchestration/plugin/commands/gate-approve.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/handoff.md",
            content: include_str!("../../../orchestration/plugin/commands/handoff.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/merge.md",
            content: include_str!("../../../orchestration/plugin/commands/merge.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/plan.md",
            content: include_str!("../../../orchestration/plugin/commands/plan.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/review.md",
            content: include_str!("../../../orchestration/plugin/commands/review.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/segment-done.md",
            content: include_str!("../../../orchestration/plugin/commands/segment-done.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/status-check.md",
            content: include_str!("../../../orchestration/plugin/commands/status-check.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/status.md",
            content: include_str!("../../../orchestration/plugin/commands/status.md"),
        },
        BundledFile {
            relative_path: "plugin/commands/update-changelog.md",
            content: include_str!("../../../orchestration/plugin/commands/update-changelog.md"),
        },
        // Skills — shared
        BundledFile {
            relative_path: "plugin/skills/shared-harness-protocol/SKILL.md",
            content: include_str!(
                "../../../orchestration/plugin/skills/shared-harness-protocol/SKILL.md"
            ),
        },
        // Skills — forge
        BundledFile {
            relative_path: "plugin/skills/forge-coding/SKILL.md",
            content: include_str!("../../../orchestration/plugin/skills/forge-coding/SKILL.md"),
        },
        BundledFile {
            relative_path: "plugin/skills/forge-planning/SKILL.md",
            content: include_str!("../../../orchestration/plugin/skills/forge-planning/SKILL.md"),
        },
        // Skills — sentinel
        BundledFile {
            relative_path: "plugin/skills/sentinel-review/SKILL.md",
            content: include_str!("../../../orchestration/plugin/skills/sentinel-review/SKILL.md"),
        },
        BundledFile {
            relative_path: "plugin/skills/sentinel-criteria/SKILL.md",
            content: include_str!(
                "../../../orchestration/plugin/skills/sentinel-criteria/SKILL.md"
            ),
        },
        // Skills — vessel
        BundledFile {
            relative_path: "plugin/skills/vessel-merge-protocol/SKILL.md",
            content: include_str!(
                "../../../orchestration/plugin/skills/vessel-merge-protocol/SKILL.md"
            ),
        },
        BundledFile {
            relative_path: "plugin/skills/vessel-ci-gate/SKILL.md",
            content: include_str!("../../../orchestration/plugin/skills/vessel-ci-gate/SKILL.md"),
        },
        // Skills — lore
        BundledFile {
            relative_path: "plugin/skills/lore-documentation/SKILL.md",
            content: include_str!(
                "../../../orchestration/plugin/skills/lore-documentation/SKILL.md"
            ),
        },
        BundledFile {
            relative_path: "plugin/skills/lore-changelog/SKILL.md",
            content: include_str!("../../../orchestration/plugin/skills/lore-changelog/SKILL.md"),
        },
    ]
}

/// One file of the artifacts tree, at the path a workspace reads it from.
pub struct TreeFile {
    pub relative_path: String,
    pub content: String,
}

/// The complete artifacts tree, in the layout the role templates consume.
///
/// The same tree is written to the shared volume by the controller and served
/// as an archive by the Manager, so both paths hand a workspace identical
/// content: `plugin/hooks/<role>/` is where hooks are installed from, and every
/// asset is mirrored under `orchestration/` with one generated `.skills`
/// manifest per registry role.
pub fn tree() -> Vec<TreeFile> {
    let mut tree = Vec::new();
    for file in files() {
        tree.push(TreeFile {
            relative_path: format!("orchestration/{}", file.relative_path),
            content: file.content.to_string(),
        });
        if file.relative_path.starts_with("plugin/hooks/") {
            tree.push(TreeFile {
                relative_path: file.relative_path.to_string(),
                content: file.content.to_string(),
            });
        }
    }
    for (role, skills) in registry_roles() {
        tree.push(TreeFile {
            relative_path: format!("orchestration/agent/roles/{role}.skills"),
            content: skills,
        });
    }
    tree
}

/// One `<role>.skills` manifest per registry role, holding the skills that role
/// is granted. The registry is compiled in, so invalid JSON is a test failure,
/// never a runtime condition.
fn registry_roles() -> Vec<(String, String)> {
    let registry = files()
        .into_iter()
        .find(|file| file.relative_path == "agent/registry.json")
        .expect("the embedded bundle must contain agent/registry.json");
    let registry: serde_json::Value =
        serde_json::from_str(registry.content).expect("the embedded registry must be valid JSON");
    registry["team"]
        .as_array()
        .expect("the embedded registry must have a team array")
        .iter()
        .map(|role| {
            let id = role["id"]
                .as_str()
                .expect("a registry role must have an id");
            let skills = role["skills"]
                .as_array()
                .expect("a registry role must have skills")
                .iter()
                .map(|skill| skill.as_str().expect("a registry skill must be a string"))
                .collect::<Vec<_>>()
                .join("\n");
            (id.to_string(), format!("{skills}\n"))
        })
        .collect()
}

/// Serialize the artifacts tree as a deterministic gzipped tar.
///
/// Entries are sorted by path and carry a zeroed mtime/uid/gid so the same
/// source always produces the same bytes, which is what makes [`digest`] a
/// stable identifier for a release.
pub fn archive() -> std::io::Result<Vec<u8>> {
    let mut tree = tree();
    tree.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));

    let encoder = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for file in tree {
        let mut header = tar::Header::new_gnu();
        header.set_path(&file.relative_path)?;
        header.set_size(file.content.len() as u64);
        // Hooks are executed directly; everything else is read as data.
        header.set_mode(if file.relative_path.ends_with(".sh") {
            0o755
        } else {
            0o644
        });
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        builder.append(&header, file.content.as_bytes())?;
    }
    let encoder = builder.into_inner()?;
    encoder.finish()
}

/// The content address of a bundle: lowercase hex SHA-256 of its archive bytes.
pub fn digest(archive: &[u8]) -> String {
    let hash = Sha256::digest(archive);
    let mut hex = String::with_capacity(hash.len() * 2);
    for byte in hash {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Vec<String> {
        tree().into_iter().map(|file| file.relative_path).collect()
    }

    #[test]
    fn tree_carries_the_paths_the_templates_install() {
        let paths = paths();
        for required in [
            "plugin/hooks/forge/session_start.sh",
            "orchestration/agent/agents/forge.agent.md",
            "orchestration/agent/roles/sentinel.skills",
            "orchestration/plugin/commands/plan.md",
            "orchestration/agent/standards/CODING.md",
        ] {
            assert!(
                paths.iter().any(|path| path == required),
                "tree is missing {required}"
            );
        }
    }

    #[test]
    fn role_manifests_match_the_registry() {
        let manifests: Vec<_> = tree()
            .into_iter()
            .filter(|file| file.relative_path.ends_with(".skills"))
            .collect();
        assert!(!manifests.is_empty());
        let sentinel = manifests
            .iter()
            .find(|file| file.relative_path == "orchestration/agent/roles/sentinel.skills")
            .expect("sentinel manifest");
        assert_eq!(
            sentinel.content,
            "sentinel-review\nsentinel-criteria\nshared-harness-protocol\n"
        );
    }

    #[test]
    fn archive_is_deterministic_and_digest_identifies_it() {
        let first = archive().unwrap();
        let second = archive().unwrap();
        assert_eq!(first, second, "the same bundle must serialize identically");
        assert_eq!(digest(&first).len(), 64);
        assert!(digest(&first).chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(digest(&first), digest(b"different bytes"));
    }

    #[test]
    fn archive_extracts_to_the_artifacts_layout() {
        let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(archive().unwrap()));
        let mut archive = tar::Archive::new(decoder);
        let mut entries = Vec::new();
        for entry in archive.entries().unwrap() {
            entries.push(
                entry
                    .unwrap()
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        assert!(entries.iter().any(|path| path.starts_with("plugin/hooks/")));
        assert!(entries
            .iter()
            .any(|path| path == "orchestration/agent/roles/forge.skills"));
        assert!(entries
            .iter()
            .any(|path| path == "orchestration/agent/agents/nexus.agent.md"));
    }
}
