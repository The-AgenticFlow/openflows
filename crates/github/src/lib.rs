// crates/github/src/lib.rs
//
// github crate — Tool schema library, MCP server spawn helpers, and REST API client.
// - MCP: High-level operations via subprocess (used by most agents)
// - REST: Low-latency direct API calls (used by VESSEL for polling/merge)

pub mod client;
pub mod rest;
pub mod schemas;

pub use client::McpGithubClient;
pub use rest::{
    CheckAnnotationDetail, CiFailureDetail, FailedCheck, GitHubIssueResponse, GithubRestClient,
};
pub use schemas::github_mcp_cmd;
