// crates/pocketflow-core/src/pair_keys.rs
//! SharedStore key schema for pair artifacts.
//!
//! Defines a consistent key namespace for FORGE-SENTINEL pair coordination
//! data stored in SharedStore (Redis or in-memory). These keys enable
//! cross-workspace coordination when pairs run in Coder workspaces that
//! don't share a filesystem.

pub mod pair_keys {
    pub fn status(pair_id: &str) -> String {
        format!("pair:{pair_id}:status")
    }

    pub fn worklog(pair_id: &str) -> String {
        format!("pair:{pair_id}:worklog")
    }

    pub fn ticket(pair_id: &str) -> String {
        format!("pair:{pair_id}:ticket")
    }

    pub fn task(pair_id: &str) -> String {
        format!("pair:{pair_id}:task")
    }

    pub fn plan(pair_id: &str) -> String {
        format!("pair:{pair_id}:plan")
    }

    pub fn contract(pair_id: &str) -> String {
        format!("pair:{pair_id}:contract")
    }

    pub fn handoff(pair_id: &str) -> String {
        format!("pair:{pair_id}:handoff")
    }

    pub fn segment_eval(pair_id: &str, n: usize) -> String {
        format!("pair:{pair_id}:segment:{n}:eval")
    }

    pub fn final_review(pair_id: &str) -> String {
        format!("pair:{pair_id}:final_review")
    }

    pub fn error_feedback(pair_id: &str) -> String {
        format!("pair:{pair_id}:error_feedback")
    }

    pub fn conflict_resolution(pair_id: &str) -> String {
        format!("pair:{pair_id}:conflict_resolution")
    }

    pub fn ci_fix(pair_id: &str) -> String {
        format!("pair:{pair_id}:ci_fix")
    }
}

#[cfg(test)]
mod tests {
    use super::pair_keys;

    #[test]
    fn test_key_format_strings() {
        assert_eq!(pair_keys::status("pair-1"), "pair:pair-1:status");
        assert_eq!(pair_keys::worklog("pair-1"), "pair:pair-1:worklog");
        assert_eq!(pair_keys::ticket("pair-1"), "pair:pair-1:ticket");
        assert_eq!(pair_keys::task("pair-1"), "pair:pair-1:task");
        assert_eq!(pair_keys::plan("pair-1"), "pair:pair-1:plan");
        assert_eq!(pair_keys::contract("pair-1"), "pair:pair-1:contract");
        assert_eq!(pair_keys::handoff("pair-1"), "pair:pair-1:handoff");
        assert_eq!(
            pair_keys::segment_eval("pair-1", 3),
            "pair:pair-1:segment:3:eval"
        );
        assert_eq!(
            pair_keys::final_review("pair-1"),
            "pair:pair-1:final_review"
        );
        assert_eq!(
            pair_keys::error_feedback("pair-1"),
            "pair:pair-1:error_feedback"
        );
        assert_eq!(
            pair_keys::conflict_resolution("pair-1"),
            "pair:pair-1:conflict_resolution"
        );
        assert_eq!(pair_keys::ci_fix("pair-1"), "pair:pair-1:ci_fix");
    }

    #[test]
    fn test_no_collisions() {
        let keys = [
            pair_keys::status("p"),
            pair_keys::worklog("p"),
            pair_keys::ticket("p"),
            pair_keys::task("p"),
            pair_keys::plan("p"),
            pair_keys::contract("p"),
            pair_keys::handoff("p"),
            pair_keys::segment_eval("p", 1),
            pair_keys::final_review("p"),
        ];
        for (i, k1) in keys.iter().enumerate() {
            for (j, k2) in keys.iter().enumerate() {
                if i != j {
                    assert_ne!(k1, k2, "Key collision: {} == {}", k1, k2);
                }
            }
        }
    }
}
