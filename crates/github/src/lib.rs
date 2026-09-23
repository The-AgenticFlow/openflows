// crates/github/src/lib.rs
//
// github crate — REST API client for GitHub.
// - REST: Direct API calls (used by Nexus for issue/PR sync and by VESSEL).

pub mod rest;

pub use rest::{
    effective_review_state, CheckAnnotationDetail, CiFailureDetail, FailedCheck,
    GitHubIssueResponse, GithubRestClient, PrReview, PrReviewState, ReviewComment,
    ReviewCommentInput,
};
