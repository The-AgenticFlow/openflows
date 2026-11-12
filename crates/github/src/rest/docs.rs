//! GitHub lookups for LORE docs PRs: branch heads, the default branch, the
//! open docs PR into a base, and the complete list of paths a PR or a
//! comparison changes (both sides of renames, never a truncated list).

use super::*;

#[derive(Deserialize)]
pub(super) struct PrFileResponse {
    pub(super) filename: String,
    pub(super) status: String,
    /// The source path of a rename.
    #[serde(default)]
    previous_filename: Option<String>,
}

/// `Some(ticket)` for a LORE docs branch, which names its ticket exactly: free
/// text in a docs PR title (which may mention other tickets) must never
/// override it. `None` for any other branch.
pub(super) fn docs_branch_ticket(branch: &str) -> Option<Option<String>> {
    branch
        .starts_with(config::docs_lifecycle::DOCS_BRANCH_PREFIX)
        .then(|| config::docs_lifecycle::ticket_from_docs_branch(branch).map(str::to_string))
}

impl PrFileResponse {
    /// Every path the change touches: both sides of a rename.
    fn paths(self) -> impl Iterator<Item = String> {
        std::iter::once(self.filename).chain(self.previous_filename)
    }
}

/// GitHub lists at most this many files for a PR or a comparison.
const MAX_LISTED_FILES: usize = 3000;
/// The compare endpoint returns at most this many files, unpaginated.
const MAX_COMPARE_FILES: usize = 300;

impl GithubRestClient {
    /// The open PR from `branch` in this repository into `base`, if any. A PR
    /// from the same branch into another base is not returned.
    pub async fn find_open_pr_by_head(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
    ) -> Result<Option<PrInfo>> {
        let url = format!(
            "{}/repos/{}/{}/pulls?state=open&head={}:{}&base={}&per_page=10",
            self.api_base, owner, repo, owner, branch, base
        );
        let resp: Vec<PullRequestResponse> = self.get_json(&url).await?;
        Ok(resp
            .into_iter()
            .find(|pr| pr.head.ref_field == branch && pr.base.ref_field == base)
            .map(|pr| PrInfo {
                number: pr.number,
                head_sha: pr.head.sha,
                ticket_id: extract_ticket_id(&pr.title, &pr.body, &pr.head.ref_field),
                head_branch: pr.head.ref_field,
                base_branch: pr.base.ref_field,
                title: pr.title,
                body: pr.body,
                state: PrState::Open,
                mergeable: pr.mergeable,
            }))
    }

    /// The commit `branch` points at on GitHub, or `None` when it does not exist.
    pub async fn get_branch_head(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<Option<String>> {
        let url = format!(
            "{}/repos/{}/{}/git/ref/heads/{}",
            self.api_base, owner, repo, branch
        );
        let resp = self.send_with_retry(|| self.build_get(&url)).await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API error {}: {}", status, body);
        }
        let value: serde_json::Value = resp.json().await?;
        Ok(value["object"]["sha"].as_str().map(str::to_string))
    }

    /// Every path PR `pr_number` changes, including the source of each rename.
    /// Fails rather than return a list GitHub may have truncated.
    pub async fn list_pr_file_paths(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<String>> {
        let mut paths = Vec::new();
        let mut listed = 0usize;
        for page in 1.. {
            let url = format!(
                "{}/repos/{}/{}/pulls/{}/files?per_page=100&page={}",
                self.api_base, owner, repo, pr_number, page
            );
            let batch: Vec<PrFileResponse> = self.get_json(&url).await?;
            let len = batch.len();
            listed += len;
            paths.extend(batch.into_iter().flat_map(PrFileResponse::paths));
            if len < 100 {
                break;
            }
            anyhow::ensure!(
                listed < MAX_LISTED_FILES,
                "PR #{pr_number} changes {MAX_LISTED_FILES}+ files; GitHub cannot list them all"
            );
        }
        Ok(paths)
    }

    /// Every path `head` changes relative to its merge base with `base` (what a
    /// PR from `head` into `base` would merge), including the source of each
    /// rename. `None` when GitHub's list may be truncated
    /// (300 or more files, the most the compare endpoint lists).
    pub async fn compare_file_paths(
        &self,
        owner: &str,
        repo: &str,
        base: &str,
        head: &str,
    ) -> Result<Option<Vec<String>>> {
        #[derive(Deserialize)]
        struct Comparison {
            #[serde(default)]
            files: Vec<PrFileResponse>,
        }
        // `per_page=1` keeps the commit list small; the file list is complete
        // on the first page regardless.
        let url = format!(
            "{}/repos/{}/{}/compare/{}...{}?per_page=1",
            self.api_base, owner, repo, base, head
        );
        let comparison: Comparison = self.get_json(&url).await?;
        if comparison.files.len() >= MAX_COMPARE_FILES {
            return Ok(None);
        }
        Ok(Some(
            comparison
                .files
                .into_iter()
                .flat_map(PrFileResponse::paths)
                .collect(),
        ))
    }

    /// The repository's default branch.
    pub async fn get_default_branch(&self, owner: &str, repo: &str) -> Result<String> {
        let url = format!("{}/repos/{}/{}", self.api_base, owner, repo);
        let value = self.get_json_raw(&url).await?;
        value["default_branch"]
            .as_str()
            .filter(|branch| !branch.is_empty())
            .map(str::to_string)
            .context("GitHub repository has no default branch")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docs_branches_name_their_ticket_over_title_text() {
        let title = "docs: follow-up to T-12 retries (#40)";
        assert_eq!(
            extract_ticket_id(title, &None, "lore/T-9").as_deref(),
            Some("T-9")
        );
        assert_eq!(
            extract_ticket_id(title, &None, "lore/T-9--c2").as_deref(),
            Some("T-9")
        );
        assert_eq!(extract_ticket_id(title, &None, "lore/a/b"), None);
        assert_eq!(
            extract_ticket_id(title, &None, "forge-1/T-9").as_deref(),
            Some("T-12")
        );
    }

    #[tokio::test]
    async fn branch_head_and_default_branch_lookups() {
        let mut server = mockito::Server::new_async().await;
        let _ref = server
            .mock("GET", "/repos/o/r/git/ref/heads/lore/T-9")
            .with_status(200)
            .with_body(r#"{"object":{"sha":"abc"}}"#)
            .create_async()
            .await;
        let _missing = server
            .mock("GET", "/repos/o/r/git/ref/heads/lore/T-10")
            .with_status(404)
            .create_async()
            .await;
        let _repo = server
            .mock("GET", "/repos/o/r")
            .with_status(200)
            .with_body(r#"{"default_branch":"trunk"}"#)
            .create_async()
            .await;
        let client = GithubRestClient::with_api_base("t", server.url());
        assert_eq!(
            client
                .get_branch_head("o", "r", "lore/T-9")
                .await
                .unwrap()
                .as_deref(),
            Some("abc")
        );
        assert_eq!(
            client.get_branch_head("o", "r", "lore/T-10").await.unwrap(),
            None
        );
        assert_eq!(client.get_default_branch("o", "r").await.unwrap(), "trunk");
    }

    #[tokio::test]
    async fn changed_paths_include_both_sides_of_renames() {
        let mut server = mockito::Server::new_async().await;
        let files = r#"[{"filename":"docs/a.md","status":"added"},{"filename":"docs/lib.md","status":"renamed","previous_filename":"src/lib.rs"}]"#;
        let _pr = server
            .mock("GET", "/repos/o/r/pulls/5/files?per_page=100&page=1")
            .with_status(200)
            .with_body(files)
            .create_async()
            .await;
        let _compare = server
            .mock("GET", "/repos/o/r/compare/main...abc?per_page=1")
            .with_status(200)
            .with_body(format!(r#"{{"files":{files}}}"#))
            .create_async()
            .await;
        let client = GithubRestClient::with_api_base("t", server.url());
        let expected = vec!["docs/a.md", "docs/lib.md", "src/lib.rs"];
        assert_eq!(
            client.list_pr_file_paths("o", "r", 5).await.unwrap(),
            expected
        );
        assert_eq!(
            client
                .compare_file_paths("o", "r", "main", "abc")
                .await
                .unwrap()
                .unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn truncated_comparisons_are_reported() {
        let mut server = mockito::Server::new_async().await;
        let files: Vec<String> = (0..MAX_COMPARE_FILES)
            .map(|i| format!(r#"{{"filename":"docs/{i}.md","status":"added"}}"#))
            .collect();
        let _compare = server
            .mock("GET", "/repos/o/r/compare/main...abc?per_page=1")
            .with_status(200)
            .with_body(format!(r#"{{"files":[{}]}}"#, files.join(",")))
            .create_async()
            .await;
        let client = GithubRestClient::with_api_base("t", server.url());
        assert_eq!(
            client
                .compare_file_paths("o", "r", "main", "abc")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn open_docs_prs_are_found_only_into_the_wanted_base() {
        let mut server = mockito::Server::new_async().await;
        let _prs = server
            .mock("GET", "/repos/o/r/pulls")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"[{"number":3,"title":"docs","body":null,"head":{"sha":"abc","ref":"lore/T-9"},"base":{"ref":"release/1","sha":"b"},"state":"open"}]"#)
            .create_async()
            .await;
        let client = GithubRestClient::with_api_base("t", server.url());
        assert!(client
            .find_open_pr_by_head("o", "r", "lore/T-9", "main")
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            client
                .find_open_pr_by_head("o", "r", "lore/T-9", "release/1")
                .await
                .unwrap()
                .unwrap()
                .number,
            3
        );
    }
}
