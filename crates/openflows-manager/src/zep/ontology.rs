//! The custom OpenFlows graph ontology for Zep.
//!
//! Zep builds temporal knowledge graphs by extracting entities and edges from
//! ingested data. Declaring a domain ontology focuses extraction on the nouns
//! and relationships that matter to an engineering organization, so the agents
//! can recall precise, structured context (which repository a decision was made
//! in, which conventions a codebase follows, where an incident occurred, and so
//! on).
//!
//! Entity types: `CodeRepository`, `TechnicalDecision`, `CodingConvention`,
//! `RuntimeEnvironment`, `EngineeringIncident`.
//! Edge types: `WORKS_ON`, `DECIDED_IN`, `FOLLOWS_CONVENTION`, `RUNS_IN`, `OWNS`.
//!
//! These are applied project-wide via `PUT /graph/set-ontology` when Zep is
//! enabled, and the shared organizational knowledge graph (`ORG_GRAPH_ID`) is
//! seeded with the same ontology.

use super::{EdgeTypeDef, EntityTypeDef, SetOntologyRequest, SourceTarget};
use serde_json::{json, Map, Value};

/// The shared organization-knowledge graph id. Every user's agent can draw on
/// knowledge added to this graph: policies, product facts, and internal docs.
pub const ORG_GRAPH_ID: &str = "zep_org_knowledge_900c667bd339";

/// Declared entity type names.
pub const ENTITY_TYPES: &[&str] = &[
    "CodeRepository",
    "TechnicalDecision",
    "CodingConvention",
    "RuntimeEnvironment",
    "EngineeringIncident",
];

/// Declared edge type names.
pub const EDGE_TYPES: &[&str] = &[
    "WORKS_ON",
    "DECIDED_IN",
    "FOLLOWS_CONVENTION",
    "RUNS_IN",
    "OWNS",
];

fn entity(name: &str, description: &str, fields: Vec<(&str, Value)>) -> (String, Value) {
    (
        name.to_string(),
        serde_json::to_value(EntityTypeDef {
            description: description.to_string(),
            fields: map(fields),
        })
        .expect("entity type is serializable"),
    )
}

fn edge(
    name: &str,
    description: &str,
    fields: Vec<(&str, Value)>,
    source_targets: Vec<SourceTarget>,
) -> (String, Value) {
    (
        name.to_string(),
        serde_json::to_value(EdgeTypeDef {
            description: description.to_string(),
            fields: map(fields),
            source_targets: Some(source_targets),
        })
        .expect("edge type is serializable"),
    )
}

fn map(fields: Vec<(&str, Value)>) -> Map<String, Value> {
    fields
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

/// A `text` field definition for an entity/edge type.
fn text(description: &str) -> Value {
    json!({ "type": "text", "description": description })
}

/// Build the full ontology request (entities + edges) applied project-wide.
pub fn ontology_request() -> SetOntologyRequest {
    let mut entities = Map::new();
    for (name, def) in [
        entity(
            "CodeRepository",
            "A source-code repository owned or worked on by the organization.",
            vec![(
                "language",
                text("The primary programming language, e.g. Rust, TypeScript."),
            )],
        ),
        entity(
            "TechnicalDecision",
            "A recorded architectural or engineering decision, with rationale and trade-offs.",
            vec![
                (
                    "status",
                    text("Decision status: proposed, accepted, rejected, superseded."),
                ),
                (
                    "rationale",
                    text("Why the decision was made and the trade-offs considered."),
                ),
            ],
        ),
        entity(
            "CodingConvention",
            "A coding standard, style rule, or engineering convention the team follows.",
            vec![(
                "scope",
                text("Where the convention applies, e.g. repository, language, or team."),
            )],
        ),
        entity(
            "RuntimeEnvironment",
            "A runtime, deployment, or hosting environment where code runs.",
            vec![(
                "provider",
                text("The provider or platform, e.g. Coder, Docker, AWS."),
            )],
        ),
        entity(
            "EngineeringIncident",
            "A production incident or outage affecting a service or repository.",
            vec![("severity", text("Impact severity, e.g. sev1, sev2."))],
        ),
    ] {
        entities.insert(name, def);
    }

    let mut edges = Map::new();
    for (name, def) in [
        edge(
            "WORKS_ON",
            "A developer or team works on a code repository.",
            vec![(
                "role",
                text("The role of the person or team in the repository, e.g. maintainer."),
            )],
            vec![SourceTarget {
                source: None,
                target: Some("CodeRepository".into()),
            }],
        ),
        edge(
            "DECIDED_IN",
            "A technical decision was made within a code repository.",
            vec![],
            vec![SourceTarget {
                source: Some("TechnicalDecision".into()),
                target: Some("CodeRepository".into()),
            }],
        ),
        edge(
            "FOLLOWS_CONVENTION",
            "A code repository follows a coding convention.",
            vec![],
            vec![SourceTarget {
                source: Some("CodeRepository".into()),
                target: Some("CodingConvention".into()),
            }],
        ),
        edge(
            "RUNS_IN",
            "A code repository runs in a runtime environment.",
            vec![],
            vec![SourceTarget {
                source: Some("CodeRepository".into()),
                target: Some("RuntimeEnvironment".into()),
            }],
        ),
        edge(
            "OWNS",
            "A team or developer owns a code repository, decision, or environment.",
            vec![],
            vec![SourceTarget {
                source: None,
                target: None,
            }],
        ),
    ] {
        edges.insert(name, def);
    }

    SetOntologyRequest {
        entities,
        edges,
        user_ids: None,
        graph_ids: None,
    }
}

/// Build an ontology request scoped to the shared org knowledge graph only.
pub fn org_graph_ontology_request() -> SetOntologyRequest {
    let mut req = ontology_request();
    req.user_ids = None;
    req.graph_ids = Some(vec![ORG_GRAPH_ID.to_string()]);
    req
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_graph_id_is_stable() {
        assert_eq!(ORG_GRAPH_ID, "zep_org_knowledge_900c667bd339");
    }

    #[test]
    fn ontology_declares_all_expected_types() {
        let req = ontology_request();
        for t in ENTITY_TYPES {
            assert!(req.entities.contains_key(*t), "missing entity {t}");
        }
        for t in EDGE_TYPES {
            assert!(req.edges.contains_key(*t), "missing edge {t}");
        }
        assert_eq!(req.entities.len(), ENTITY_TYPES.len());
        assert_eq!(req.edges.len(), EDGE_TYPES.len());
    }

    #[test]
    fn ontology_serializes_to_valid_json() {
        let req = ontology_request();
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("entities").is_some());
        assert!(json.get("edges").is_some());
        // Edges carry source/target constraints.
        let edges = json["edges"].as_object().unwrap();
        let decided_in = edges["DECIDED_IN"].as_object().unwrap();
        assert!(decided_in.contains_key("source_targets"));
    }

    #[test]
    fn org_ontology_is_scoped_to_the_org_graph() {
        let req = org_graph_ontology_request();
        assert_eq!(
            req.graph_ids.as_deref(),
            Some(&[ORG_GRAPH_ID.to_string()][..])
        );
        assert!(req.user_ids.is_none());
    }
}
