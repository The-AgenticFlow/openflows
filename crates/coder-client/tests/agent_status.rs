use coder_client::{AgentStatus, CoderWorkspace};

fn workspace(status: &str, lifecycle: &str) -> CoderWorkspace {
    let json = format!(
        r#"{{"id":"w","latest_build":{{"status":"running","resources":[{{"agents":[{{"name":"main","status":"{status}","lifecycle_state":"{lifecycle}"}}]}}]}}}}"#
    );
    serde_json::from_str(&json).expect("a workspace always decodes")
}

#[test]
fn a_connecting_agent_decodes_and_is_not_ready() {
    // Coder reports `connecting` while the agent starts; the readiness poll must survive it.
    let ws = workspace("connecting", "starting");
    assert_eq!(ws.agent_status(), AgentStatus::Connecting);
    assert!(!ws.is_agent_ready());
}

#[test]
fn only_the_agent_status_keeps_a_connecting_agent_from_being_ready() {
    // A ready lifecycle on a running build leaves the status as the only thing to say no,
    // and a connected agent in the same state is the control that says yes.
    assert!(!workspace("connecting", "ready").is_agent_ready());
    assert!(workspace("connected", "ready").is_agent_ready());
}

#[test]
fn an_agent_status_coder_adds_later_falls_back_instead_of_failing() {
    assert_eq!(
        workspace("brand_new_state", "ready").agent_status(),
        AgentStatus::Unknown("brand_new_state".into())
    );
    assert_eq!(
        workspace("connected", "ready").agent_status(),
        AgentStatus::Connected
    );
}
