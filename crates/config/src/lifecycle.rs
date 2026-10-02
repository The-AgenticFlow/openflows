//! Authoritative ticket lifecycle. Scheduling/worker states are projections.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Planning,
    PlanReady,
    PlanRejected,
    Building,
    Testing,
    Submit,
    Done,
    Blocked,
}
impl Phase {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(serde_json::from_value(serde_json::Value::String(
            s.to_owned(),
        ))?)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::PlanReady => "plan_ready",
            Self::PlanRejected => "plan_rejected",
            Self::Building => "building",
            Self::Testing => "testing",
            Self::Submit => "submit",
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Decision {
    #[serde(default)]
    pub pr_number: Option<u64>,
    pub round: u64,
    pub actor: String,
    pub approved: bool,
    pub report: String,
    pub revision: u64,
    pub head: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Lifecycle {
    pub history: Vec<TransitionRecord>,
    pub merge_pending: bool,
    pub pr_delivery: Option<Decision>,
    pub review_round: u64,
    pub version: u64,
    pub phase: Phase,
    pub role: String,
    pub ts: u64,
    pub plan: String,
    pub revision: u64,
    pub head: Option<String>,
    pub plan_decision: Option<Decision>,
    pub test_decision: Option<Decision>,
    pub test_human: Option<Decision>,
    pub pr_decision: Option<Decision>,
    pub pr_human: Option<Decision>,
    pub verified_head: Option<String>,
    pub verification_task: Option<String>,
    pub pr_number: Option<u64>,
    pub merge_sha: Option<String>,
    pub feedback: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransitionRecord {
    pub version: u64,
    pub actor: String,
    pub from: Phase,
    pub to: Phase,
    pub ts: u64,
    pub detail: String,
}
#[derive(Debug, Clone)]
pub enum Event {
    Move {
        phase: Phase,
        head: Option<String>,
    },
    Plan {
        content: String,
    },
    Decide {
        round: u64,
        phase: Phase,
        approved: bool,
        report: String,
        revision: u64,
        head: Option<String>,
    },
    Verified {
        head: String,
        task: String,
    },
    Pr {
        number: u64,
    },
    BeginMerge {
        head: String,
    },
    CancelMerge,
    ReviewDelivered {
        round: u64,
    },
    ReconcileMerged {
        number: u64,
        sha: String,
    },
    Merged {
        number: u64,
        head: String,
        sha: String,
    },
}
impl Lifecycle {
    /// Upgrade legacy records conservatively: unverifiable approvals never survive.
    pub fn decode(value: Option<serde_json::Value>) -> Result<Self> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        if value.get("version").is_some() {
            let mut state: Self = serde_json::from_value(value)?;
            // Older outbox entries did not identify their PR. They cannot be
            // safely delivered or used as proof of native review delivery.
            if state
                .pr_delivery
                .as_ref()
                .is_some_and(|d| d.pr_number.is_none())
            {
                state.pr_delivery = None;
                state.pr_decision = None;
                state.pr_human = None;
            }
            return Ok(state);
        }
        let legacy = value
            .as_str()
            .or_else(|| value.get("phase").and_then(|p| p.as_str()));
        ensure!(
            legacy.is_some(),
            "Malformed lifecycle record; refusing to reset it"
        );
        Ok(Self {
            phase: if matches!(legacy, Some("Merged" | "merged" | "done")) {
                Phase::Done
            } else {
                Phase::Planning
            },
            feedback: Some("Legacy state requires a new plan review".into()),
            ..Self::default()
        })
    }
    fn clear_work(&mut self) {
        self.head = None;
        self.verified_head = None;
        self.verification_task = None;
        self.test_decision = None;
        self.test_human = None;
        self.pr_decision = None;
        self.pr_human = None;
    }
    pub fn apply(&self, actor: &str, event: Event, ts: u64) -> Result<Self> {
        ensure!(self.phase != Phase::Done, "Done is terminal");
        let actor = actor.to_ascii_lowercase();
        ensure!(
            !self.merge_pending
                || matches!(
                    &event,
                    Event::Merged { .. } | Event::CancelMerge | Event::ReconcileMerged { .. }
                ),
            "Merge is in progress; reconcile before changing the candidate"
        );
        let detail = match &event {
            Event::Decide {
                approved,
                report,
                round,
                ..
            } => format!(
                "review round {round}: {}: {report}",
                if *approved { "approve" } else { "reject" }
            ),
            Event::Plan { .. } => format!("plan revision {} uploaded", self.revision + 1),
            _ => format!("{event:?}"),
        };
        let mut next = self.clone();
        match event {
            Event::Plan { content } => {
                ensure!(
                    actor == "forge" && self.phase == Phase::Planning,
                    "Plans may only be edited by FORGE in planning"
                );
                ensure!(!content.trim().is_empty(), "Plan must not be empty");
                next.plan = content;
                next.revision += 1;
                next.plan_decision = None;
                next.clear_work();
            }
            Event::Move { phase, head } => {
                ensure!(
                    actor == "forge"
                        || (actor == "vessel" && phase == Phase::Building)
                        || (actor == "sentinel"
                            && self.phase == Phase::Testing
                            && phase == Phase::Blocked),
                    "Actor cannot move lifecycle"
                );
                if self.phase == phase {
                    return Ok(self.clone());
                }
                let permitted = match (self.phase, phase) {
                    (_, Phase::Blocked) => true,
                    (
                        Phase::Blocked
                        | Phase::PlanRejected
                        | Phase::PlanReady
                        | Phase::Building
                        | Phase::Testing
                        | Phase::Submit,
                        Phase::Planning,
                    ) => true,
                    (Phase::Planning, Phase::PlanReady) => {
                        !self.plan.trim().is_empty() && self.revision > 0
                    }
                    (Phase::PlanReady, Phase::Building) => self
                        .plan_decision
                        .as_ref()
                        .is_some_and(|d| d.approved && d.revision == self.revision),
                    (Phase::Building, Phase::Testing) => {
                        head.as_ref().is_some_and(|s| !s.trim().is_empty())
                    }
                    (Phase::Testing, Phase::Submit) => {
                        // TODO(human-testing-review): require explicit human approval
                        // here once that workflow is implemented. Do not fabricate it.
                        self.test_decision.as_ref().is_some_and(|d| {
                            d.approved
                                && d.revision == self.revision
                                && d.round == self.review_round
                                && d.head == self.head
                        }) && self.verified_head == self.head
                            && self.verification_task.is_some()
                            && self.head.is_some()
                    }
                    (Phase::Testing | Phase::Submit, Phase::Building) => true,
                    _ => false,
                };
                ensure!(
                    permitted,
                    "Invalid or ungated transition {} -> {}",
                    self.phase.as_str(),
                    phase.as_str()
                );
                if matches!(phase, Phase::Planning | Phase::Blocked) {
                    next.clear_work();
                    next.plan_decision = None;
                    if phase == Phase::Planning {
                        next.plan.clear();
                    }
                }
                if phase == Phase::Building {
                    next.clear_work();
                }
                if phase == Phase::Testing {
                    next.head = head;
                }
                if matches!(phase, Phase::PlanReady | Phase::Testing | Phase::Submit) {
                    next.review_round += 1;
                }
                // Moving on supersedes queued notification of an older review.
                // Its report remains in history; GitHub availability cannot
                // prevent rework from reaching a new testing round.
                next.pr_delivery = None;
                next.feedback = None;
                next.phase = phase;
            }
            Event::Decide {
                round,
                phase,
                approved,
                report,
                revision,
                head,
            } => {
                ensure!(
                    phase == self.phase && round == self.review_round,
                    "Review phase or round changed"
                );
                ensure!(revision == self.revision, "Stale plan revision");
                ensure!(!report.trim().is_empty(), "Review requires a report");
                ensure!(
                    matches!(actor.as_str(), "sentinel" | "human"),
                    "Only SENTINEL or a human may review"
                );
                ensure!(
                    phase != Phase::PlanReady || actor == "sentinel",
                    "SENTINEL reviews the plan"
                );
                ensure!(
                    matches!(phase, Phase::PlanReady | Phase::Testing | Phase::Submit),
                    "Not a review phase"
                );
                if phase != Phase::PlanReady {
                    ensure!(
                        head.is_some() && head == self.head,
                        "Review must identify the tested head"
                    );
                    if approved && phase == Phase::Testing {
                        ensure!(
                            self.verified_head == self.head && self.verification_task.is_some(),
                            "Successful A2A verification required"
                        );
                    }
                    if phase == Phase::Submit {
                        ensure!(
                            self.pr_number.is_some(),
                            "Record the PR before reviewing it"
                        );
                    }
                }
                let d = Decision {
                    pr_number: (phase == Phase::Submit).then_some(self.pr_number).flatten(),
                    round,
                    actor: actor.clone(),
                    approved,
                    report: report.clone(),
                    revision,
                    head,
                };
                if phase == Phase::Submit && actor == "sentinel" {
                    next.pr_delivery = Some(d.clone());
                }
                match (phase, actor.as_str()) {
                    (Phase::PlanReady, _) => next.plan_decision = Some(d),
                    (Phase::Testing, "sentinel") => next.test_decision = Some(d),
                    (Phase::Testing, _) => next.test_human = Some(d),
                    (Phase::Submit, "sentinel") => next.pr_decision = Some(d),
                    (Phase::Submit, _) => next.pr_human = Some(d),
                    _ => unreachable!(),
                }
                if approved && phase == Phase::PlanReady {
                    // The verdict and permission to build commit atomically.
                    next.phase = Phase::Building;
                    next.clear_work();
                    next.feedback = None;
                }
                if !approved {
                    // A human veto supersedes a queued SENTINEL approval.
                    if actor == "human" {
                        next.pr_delivery = None;
                    }
                    next.feedback = Some(report);
                    next.phase = if phase == Phase::PlanReady {
                        Phase::PlanRejected
                    } else {
                        Phase::Building
                    };
                    next.clear_work();
                }
            }
            Event::Verified { head, task } => {
                ensure!(
                    actor == "sentinel" && self.phase == Phase::Testing,
                    "Verification belongs to testing"
                );
                ensure!(
                    self.head.as_deref() == Some(&head) && !task.is_empty(),
                    "Verification must identify the tested head and task"
                );
                next.verified_head = Some(head);
                next.verification_task = Some(task);
            }
            Event::Pr { number } => {
                ensure!(
                    actor == "forge" && self.phase == Phase::Submit && number > 0,
                    "PR publication requires submit"
                );
                if self.pr_number != Some(number) {
                    next.pr_delivery = None;
                    next.pr_decision = None;
                    next.pr_human = None;
                    next.review_round += 1;
                }
                next.pr_number = Some(number);
            }
            Event::ReviewDelivered { round } => {
                ensure!(
                    actor == "sentinel"
                        && self.pr_delivery.as_ref().is_some_and(|d| d.round == round),
                    "No matching pending delivery"
                );
                next.pr_delivery = None;
            }
            Event::BeginMerge { head } => {
                ensure!(
                    actor == "vessel" && self.merge_ready(&head),
                    "Candidate is not ready to merge"
                );
                next.merge_pending = true;
            }
            Event::CancelMerge => {
                ensure!(actor == "vessel", "Only VESSEL reconciles a merge");
                next.merge_pending = false;
            }
            Event::ReconcileMerged { number, sha } => {
                ensure!(
                    actor == "vessel" && number > 0 && !sha.is_empty(),
                    "External merge requires GitHub evidence"
                );
                next.phase = Phase::Done;
                next.pr_number = Some(number);
                next.merge_sha = Some(sha);
                next.merge_pending = false;
            }
            Event::Merged { number, head, sha } => {
                ensure!(
                    actor == "vessel"
                        && self.merge_ready(&head)
                        && self.pr_number == Some(number)
                        && !sha.is_empty(),
                    "Merge requires reviewed current head and merge evidence"
                );
                next.phase = Phase::Done;
                next.merge_sha = Some(sha);
                next.merge_pending = false;
            }
        }
        next.version += 1;
        next.history.push(TransitionRecord {
            version: next.version,
            actor: actor.clone(),
            from: self.phase,
            to: next.phase,
            ts,
            detail,
        });
        next.role = actor;
        next.ts = ts;
        Ok(next)
    }
    pub fn deliverable_review(&self) -> Option<&Decision> {
        self.pr_delivery.as_ref().filter(|d| {
            d.pr_number.is_some()
                && d.pr_number == self.pr_number
                && d.round == self.review_round
                && d.revision == self.revision
                && if d.approved {
                    self.phase == Phase::Submit
                        && d.head == self.head
                        && self.pr_decision.as_ref() == Some(*d)
                        && self.pr_human.as_ref().is_some_and(|h| h.approved)
                } else {
                    self.phase == Phase::Building
                }
        })
    }
    pub fn merge_ready(&self, head: &str) -> bool {
        self.pr_delivery.is_none()
            && self.phase == Phase::Submit
            && self.head.as_deref() == Some(head)
            && self.pr_decision.as_ref().is_some_and(|d| d.approved)
            && self.pr_human.as_ref().is_some_and(|d| d.approved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(s: &mut Lifecycle, actor: &str, event: Event) {
        *s = s.apply(actor, event, 1).unwrap();
    }
    fn decide(s: &mut Lifecycle, actor: &str, phase: Phase, approved: bool) {
        run(
            s,
            actor,
            Event::Decide {
                round: s.review_round,
                phase,
                approved,
                report: "review evidence".into(),
                revision: s.revision,
                head: s.head.clone(),
            },
        );
    }
    #[test]
    fn sentinel_can_block_testing_but_cannot_move_to_building() {
        let state = Lifecycle {
            phase: Phase::Testing,
            head: Some("candidate".into()),
            review_round: 3,
            ..Lifecycle::default()
        };
        let blocked = state
            .apply(
                "sentinel",
                Event::Move {
                    phase: Phase::Blocked,
                    head: None,
                },
                1,
            )
            .unwrap();
        assert_eq!(blocked.phase, Phase::Blocked);
        assert_eq!(blocked.review_round, 3);
        assert!(state
            .apply(
                "sentinel",
                Event::Move {
                    phase: Phase::Building,
                    head: None
                },
                1
            )
            .is_err());
        assert!(blocked
            .apply(
                "sentinel",
                Event::Move {
                    phase: Phase::Planning,
                    head: None
                },
                1
            )
            .is_err());
    }

    #[test]
    fn plan_approval_enters_building_in_the_same_transition() {
        let mut state = Lifecycle {
            phase: Phase::PlanReady,
            plan: "# Plan".into(),
            revision: 1,
            review_round: 1,
            ..Lifecycle::default()
        };
        decide(&mut state, "sentinel", Phase::PlanReady, true);
        assert_eq!(state.phase, Phase::Building);
        assert!(state.plan_decision.as_ref().unwrap().approved);
        assert_eq!(state.version, 1);
        let transition = state.history.last().unwrap();
        assert_eq!(transition.actor, "sentinel");
        assert_eq!(transition.from, Phase::PlanReady);
        assert_eq!(transition.to, Phase::Building);
        assert!(state
            .apply(
                "sentinel",
                Event::Decide {
                    phase: Phase::PlanReady,
                    round: 1,
                    revision: 1,
                    approved: true,
                    report: "duplicate".into(),
                    head: None,
                },
                2
            )
            .is_err());
    }

    #[test]
    fn plan_approval_cannot_override_a_later_blocker() {
        let state = Lifecycle {
            phase: Phase::PlanReady,
            revision: 1,
            review_round: 1,
            plan: "# Plan".into(),
            ..Default::default()
        };
        let blocked = state
            .apply(
                "forge",
                Event::Move {
                    phase: Phase::Blocked,
                    head: None,
                },
                1,
            )
            .unwrap();
        assert!(blocked
            .apply(
                "sentinel",
                Event::Decide {
                    phase: Phase::PlanReady,
                    revision: 1,
                    round: 1,
                    approved: true,
                    report: "late review".into(),
                    head: None,
                },
                2
            )
            .is_err());
        assert_eq!(blocked.phase, Phase::Blocked);
        assert!(blocked.plan_decision.is_none());
    }

    fn building() -> Lifecycle {
        let mut s = Lifecycle::default();
        run(
            &mut s,
            "forge",
            Event::Plan {
                content: "# Plan".into(),
            },
        );
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::PlanReady,
                head: None,
            },
        );
        decide(&mut s, "sentinel", Phase::PlanReady, true);
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::Building,
                head: None,
            },
        );
        s
    }
    #[test]
    fn full_flow_requires_a2a_sentinel_testing_and_human_pr_review() {
        let mut s = building();
        assert!(s
            .apply(
                "forge",
                Event::Move {
                    phase: Phase::Submit,
                    head: None
                },
                1
            )
            .is_err());
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::Testing,
                head: Some("abc".into()),
            },
        );
        assert!(s
            .apply(
                "sentinel",
                Event::Decide {
                    round: s.review_round,
                    phase: Phase::Testing,
                    approved: true,
                    report: "ok".into(),
                    revision: s.revision,
                    head: s.head.clone()
                },
                1
            )
            .is_err());
        run(
            &mut s,
            "sentinel",
            Event::Verified {
                head: "abc".into(),
                task: "task-1".into(),
            },
        );
        assert!(
            s.apply(
                "forge",
                Event::Move {
                    phase: Phase::Submit,
                    head: None,
                },
                1
            )
            .is_err(),
            "A2A success without SENTINEL approval is insufficient"
        );
        decide(&mut s, "sentinel", Phase::Testing, true);
        assert!(s.test_human.is_none(), "human testing review is deferred");
        for mismatch in ["revision", "round", "head", "task"] {
            let mut stale = s.clone();
            match mismatch {
                "revision" => stale.test_decision.as_mut().unwrap().revision += 1,
                "round" => stale.test_decision.as_mut().unwrap().round += 1,
                "head" => stale.verified_head = Some("different-head".into()),
                "task" => stale.verification_task = None,
                _ => unreachable!(),
            }
            assert!(
                stale
                    .apply(
                        "forge",
                        Event::Move {
                            phase: Phase::Submit,
                            head: None,
                        },
                        1
                    )
                    .is_err(),
                "must reject missing or stale {mismatch} evidence"
            );
        }
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::Submit,
                head: None,
            },
        );
        run(&mut s, "forge", Event::Pr { number: 42 });
        decide(&mut s, "sentinel", Phase::Submit, true);
        assert!(!s.merge_ready("abc"));
        decide(&mut s, "human", Phase::Submit, true);
        let round = s.review_round;
        run(&mut s, "sentinel", Event::ReviewDelivered { round });
        assert!(s.merge_ready("abc"));
        assert!(!s.merge_ready("changed"));
        run(
            &mut s,
            "vessel",
            Event::Merged {
                number: 42,
                head: "abc".into(),
                sha: "merge".into(),
            },
        );
        assert_eq!(s.phase, Phase::Done);
        assert!(s
            .apply(
                "forge",
                Event::Move {
                    phase: Phase::Planning,
                    head: None
                },
                1
            )
            .is_err());
    }
    #[test]
    fn rejected_plan_requires_revision_and_new_review() {
        let mut s = Lifecycle::default();
        run(
            &mut s,
            "forge",
            Event::Plan {
                content: "plan".into(),
            },
        );
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::PlanReady,
                head: None,
            },
        );
        decide(&mut s, "sentinel", Phase::PlanReady, false);
        assert_eq!(s.phase, Phase::PlanRejected);
        assert_eq!(s.feedback.as_deref(), Some("review evidence"));
        assert!(s
            .apply(
                "forge",
                Event::Move {
                    phase: Phase::Building,
                    head: None
                },
                1
            )
            .is_err());
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::Planning,
                head: None,
            },
        );
        run(
            &mut s,
            "forge",
            Event::Plan {
                content: "revised".into(),
            },
        );
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::PlanReady,
                head: None,
            },
        );
        assert!(s
            .apply(
                "sentinel",
                Event::Decide {
                    round: s.review_round,
                    phase: Phase::PlanReady,
                    approved: true,
                    report: "old".into(),
                    revision: 1,
                    head: None
                },
                1
            )
            .is_err());
    }
    #[test]
    fn blocked_cannot_bypass_planning_and_plan_is_frozen_during_review() {
        let mut s = Lifecycle::default();
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::Blocked,
                head: None,
            },
        );
        assert!(s
            .apply(
                "forge",
                Event::Move {
                    phase: Phase::Building,
                    head: None
                },
                1
            )
            .is_err());
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::Planning,
                head: None,
            },
        );
        run(
            &mut s,
            "forge",
            Event::Plan {
                content: "plan".into(),
            },
        );
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::PlanReady,
                head: None,
            },
        );
        assert!(s
            .apply(
                "forge",
                Event::Plan {
                    content: "changed".into()
                },
                1
            )
            .is_err());
        assert!(s
            .apply(
                "forge",
                Event::Decide {
                    round: s.review_round,
                    phase: Phase::PlanReady,
                    approved: true,
                    report: "self".into(),
                    revision: s.revision,
                    head: None
                },
                1
            )
            .is_err());
    }
    #[test]
    fn testing_rejection_invalidates_head_evidence() {
        let mut s = building();
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::Testing,
                head: Some("abc".into()),
            },
        );
        run(
            &mut s,
            "sentinel",
            Event::Verified {
                head: "abc".into(),
                task: "task".into(),
            },
        );
        decide(&mut s, "sentinel", Phase::Testing, false);
        assert_eq!(s.phase, Phase::Building);
        assert!(s.verified_head.is_none());
    }
    #[test]
    fn previous_round_cannot_approve_retested_same_head() {
        let mut s = building();
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::Testing,
                head: Some("abc".into()),
            },
        );
        let stale = s.review_round;
        decide(&mut s, "human", Phase::Testing, false);
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::Testing,
                head: Some("abc".into()),
            },
        );
        run(
            &mut s,
            "sentinel",
            Event::Verified {
                head: "abc".into(),
                task: "new-task".into(),
            },
        );
        assert!(s
            .apply(
                "human",
                Event::Decide {
                    round: stale,
                    phase: Phase::Testing,
                    approved: true,
                    report: "late".into(),
                    revision: s.revision,
                    head: s.head.clone()
                },
                1
            )
            .is_err());
        assert!(s.history.iter().any(|e| e.detail.contains("reject")));
    }
    #[test]
    fn merge_reservation_blocks_rejection_until_confirmed() {
        let mut s = building();
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::Testing,
                head: Some("abc".into()),
            },
        );
        run(
            &mut s,
            "sentinel",
            Event::Verified {
                head: "abc".into(),
                task: "task".into(),
            },
        );
        decide(&mut s, "sentinel", Phase::Testing, true);
        decide(&mut s, "human", Phase::Testing, true);
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::Submit,
                head: None,
            },
        );
        run(&mut s, "forge", Event::Pr { number: 1 });
        decide(&mut s, "sentinel", Phase::Submit, true);
        decide(&mut s, "human", Phase::Submit, true);
        assert!(!s.merge_ready("abc"), "GitHub delivery is still pending");
        let round = s.review_round;
        run(&mut s, "sentinel", Event::ReviewDelivered { round });
        run(&mut s, "vessel", Event::BeginMerge { head: "abc".into() });
        assert!(s
            .apply(
                "human",
                Event::Decide {
                    round,
                    phase: Phase::Submit,
                    approved: false,
                    report: "late rejection".into(),
                    revision: s.revision,
                    head: s.head.clone()
                },
                1
            )
            .is_err());
        assert!(s
            .apply(
                "forge",
                Event::Move {
                    phase: Phase::Building,
                    head: None
                },
                1
            )
            .is_err());
    }
    fn submitted() -> Lifecycle {
        let mut s = building();
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::Testing,
                head: Some("abc".into()),
            },
        );
        run(
            &mut s,
            "sentinel",
            Event::Verified {
                head: "abc".into(),
                task: "test-1".into(),
            },
        );
        decide(&mut s, "sentinel", Phase::Testing, true);
        decide(&mut s, "human", Phase::Testing, true);
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::Submit,
                head: None,
            },
        );
        run(&mut s, "forge", Event::Pr { number: 42 });
        s
    }

    #[test]
    fn human_rejection_cancels_undelivered_approval() {
        let mut s = submitted();
        decide(&mut s, "sentinel", Phase::Submit, true);
        assert!(s.pr_delivery.is_some());
        decide(&mut s, "human", Phase::Submit, false);
        assert_eq!(s.phase, Phase::Building);
        assert!(
            s.pr_delivery.is_none(),
            "Rejected candidate must not receive pending approval"
        );
    }

    #[test]
    fn replacing_pr_cancels_old_delivery() {
        let mut s = submitted();
        decide(&mut s, "sentinel", Phase::Submit, true);
        let old_round = s.review_round;
        run(&mut s, "forge", Event::Pr { number: 43 });
        assert!(
            s.pr_delivery.is_none(),
            "Never deliver the old review to a replacement PR"
        );
        assert!(s
            .apply("sentinel", Event::ReviewDelivered { round: old_round }, 1)
            .is_err());
    }

    #[test]
    fn failed_rejection_delivery_does_not_block_retesting() {
        let mut s = submitted();
        decide(&mut s, "sentinel", Phase::Submit, false);
        assert!(s.pr_delivery.is_some());
        run(
            &mut s,
            "forge",
            Event::Move {
                phase: Phase::Testing,
                head: Some("fixed".into()),
            },
        );
        assert_eq!(s.phase, Phase::Testing);
        assert!(
            s.pr_delivery.is_none(),
            "New review round supersedes obsolete delivery"
        );
        assert!(s.history.iter().any(|h| h.detail.contains("reject")));
    }

    #[test]
    fn approval_delivery_waits_for_human_and_keeps_original_pr() {
        let mut s = submitted();
        decide(&mut s, "sentinel", Phase::Submit, true);
        assert_eq!(s.pr_delivery.as_ref().unwrap().pr_number, Some(42));
        assert!(s.deliverable_review().is_none());
        decide(&mut s, "human", Phase::Submit, true);
        assert!(s.deliverable_review().is_some());
        // A detached or old serialized delivery is never paired with the
        // current lifecycle PR number, even if a caller missed invalidation.
        s.pr_number = Some(43);
        assert!(s.deliverable_review().is_none());
    }

    #[test]
    fn unbound_legacy_delivery_cannot_authorize_merge() {
        let mut s = submitted();
        decide(&mut s, "sentinel", Phase::Submit, true);
        decide(&mut s, "human", Phase::Submit, true);
        let mut value = serde_json::to_value(&s).unwrap();
        value["pr_delivery"]
            .as_object_mut()
            .unwrap()
            .remove("pr_number");
        let migrated = Lifecycle::decode(Some(value)).unwrap();
        assert!(migrated.pr_delivery.is_none());
        assert!(!migrated.merge_ready("abc"));
        assert!(migrated.pr_decision.is_none());
    }
}
