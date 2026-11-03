//! PR creation through the brokered API-purpose GitHub credential. This path
//! works even when the optional `gh` package is unavailable in a workspace.

use anyhow::{bail, Context, Result};
use reqwest::{header, Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Deserialize)]
struct PullRequest {
    number: u64,
    title: String,
    html_url: String,
    state: String,
}

#[derive(Debug)]
pub struct CreatedPr {
    pub number: u64,
    pub title: String,
    pub url: String,
}

#[derive(Serialize)]
struct NewPullRequest<'a> {
    title: &'a str,
    head: &'a str,
    base: &'a str,
    body: &'a str,
}

fn valid_repo_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn valid_branch(value: &str) -> bool {
    !value.is_empty()
        && Command::new("git")
            .args(["check-ref-format", "--branch", value])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

fn validate<'a>(repo: &'a str, base: &str, head: &str, title: &str) -> Result<(&'a str, &'a str)> {
    let (owner, name) = repo.split_once('/').context("--repo must be OWNER/REPO")?;
    if !valid_repo_component(owner) || !valid_repo_component(name) {
        bail!("--repo must be OWNER/REPO with valid GitHub names");
    }
    if !valid_branch(base) || !valid_branch(head) {
        bail!("--base and --head must be valid branch names");
    }
    if title.trim().is_empty() {
        bail!("--title must not be empty");
    }
    Ok((owner, name))
}

fn api_token() -> Result<String> {
    let path = PathBuf::from(
        std::env::var("OPENFLOWS_GITHUB_API_TOKEN_FILE")
            .context("OPENFLOWS_GITHUB_API_TOKEN_FILE is not set")?,
    );
    api_token_from_path(&path)
}

fn api_token_from_path(path: &Path) -> Result<String> {
    let token = std::fs::read_to_string(path).with_context(|| {
        format!(
            "cannot read brokered GitHub API token at {}",
            path.display()
        )
    })?;
    let token = token.trim();
    if token.is_empty() {
        bail!("brokered GitHub API token file is empty");
    }
    Ok(token.to_owned())
}

/// Find an already-open PR first so a retry after an interrupted response does
/// not open a duplicate. A closed match is not silently resurrected.
pub async fn create_or_find(
    repo: &str,
    base: &str,
    head: &str,
    title: &str,
    body: &str,
) -> Result<CreatedPr> {
    validate(repo, base, head, title)?;
    let token = api_token()?;
    let api_base =
        std::env::var("GITHUB_API_BASE").unwrap_or_else(|_| "https://api.github.com".to_owned());
    create_or_find_with(repo, base, head, title, body, &api_base, &token).await
}

async fn create_or_find_with(
    repo: &str,
    base: &str,
    head: &str,
    title: &str,
    body: &str,
    api_base: &str,
    token: &str,
) -> Result<CreatedPr> {
    let (owner, _name) = validate(repo, base, head, title)?;
    let api_base = api_base.trim_end_matches('/');
    if !api_base.starts_with("https://") && !api_base.starts_with("http://") {
        bail!("GITHUB_API_BASE must be an HTTP(S) URL");
    }
    let url = format!("{api_base}/repos/{repo}/pulls");
    let client = Client::builder()
        .user_agent("openflows-harness")
        .default_headers({
            let mut headers = header::HeaderMap::new();
            headers.insert(header::ACCEPT, "application/vnd.github+json".parse()?);
            headers
        })
        .build()?;
    let head_filter = format!("{owner}:{head}");
    let response = client
        .get(&url)
        .bearer_auth(token)
        .query(&[("state", "all"), ("head", &head_filter), ("base", base)])
        .send()
        .await
        .context("GitHub PR lookup failed")?;
    if !response.status().is_success() {
        bail!("GitHub PR lookup failed with HTTP {}", response.status());
    }
    let existing: Vec<PullRequest> = response
        .json()
        .await
        .context("invalid GitHub PR lookup response")?;
    if let Some(pr) = existing.iter().find(|pr| pr.state == "open") {
        return Ok(CreatedPr {
            number: pr.number,
            title: pr.title.clone(),
            url: pr.html_url.clone(),
        });
    }
    if let Some(pr) = existing.first() {
        bail!(
            "PR #{} for {head} -> {base} is {}; reopen it or choose another branch",
            pr.number,
            pr.state
        );
    }

    let response = client
        .post(&url)
        .bearer_auth(token)
        .json(&NewPullRequest {
            title,
            head,
            base,
            body,
        })
        .send()
        .await
        .context("GitHub PR creation failed")?;
    if response.status() != StatusCode::CREATED {
        bail!("GitHub PR creation failed with HTTP {}; check Pull requests:Write permission and branch availability", response.status());
    }
    let pr: PullRequest = response
        .json()
        .await
        .context("invalid GitHub PR creation response")?;
    Ok(CreatedPr {
        number: pr.number,
        title: pr.title,
        url: pr.html_url,
    })
}

#[cfg(test)]
mod tests {
    use super::{api_token_from_path, create_or_find_with, validate};
    use std::path::Path;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn mock_github(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0; 4096];
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "request ended before all headers/body arrived");
                    bytes.extend_from_slice(&chunk[..n]);
                    let Some(headers_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&bytes[..headers_end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= headers_end + 4 + length {
                        break;
                    }
                }
                requests.push(String::from_utf8(bytes).unwrap());
                let reason = match status {
                    200 => "OK",
                    201 => "Created",
                    403 => "Forbidden",
                    _ => "Error",
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (base, task)
    }

    fn token_file(dir: &Path) -> String {
        let path = dir.join("github-token");
        std::fs::write(&path, "brokered-test-token\n").unwrap();
        api_token_from_path(&path).unwrap()
    }

    #[test]
    fn validates_pr_arguments_before_external_effects() {
        assert!(validate("org/repo", "main", "forge/T-003", "Ticket title").is_ok());
        assert!(validate("org/repo", "release/1.0+hotfix", "feature/a@b=c", "Title").is_ok());
        assert!(validate("org/repo/extra", "main", "forge/T-003", "Title").is_err());
        assert!(validate("org/repo", "main", "../escape", "Title").is_err());
        assert!(validate("org/repo", "main", "bad..branch", "Title").is_err());
        assert!(validate("org/repo", "main", "branch.lock", "Title").is_err());
        assert!(validate("org/repo", "main", "forge/T-003", " ").is_err());
    }

    #[tokio::test]
    async fn reuses_open_pr_with_brokered_token_without_posting() {
        let dir = tempfile::tempdir().unwrap();
        let token = token_file(dir.path());
        let (base, server) = mock_github(vec![(
            200,
            r#"[{"number":42,"title":"Existing","html_url":"https://github.test/pr/42","state":"open"}]"#,
        )])
        .await;
        let pr = create_or_find_with(
            "org/repo",
            "main",
            "forge/T-003",
            "New title",
            "New body",
            &base,
            &token,
        )
        .await
        .unwrap();
        let requests = server.await.unwrap();
        assert_eq!(pr.number, 42);
        assert_eq!(requests.len(), 1, "an existing PR must not trigger POST");
        assert!(requests[0].starts_with("GET /repos/org/repo/pulls?"));
        assert!(requests[0].contains("authorization: Bearer brokered-test-token"));
        assert!(requests[0].contains("state=all"));
        assert!(requests[0].contains("base=main"));
    }

    #[tokio::test]
    async fn creates_pr_with_descriptive_payload_and_brokered_token() {
        let dir = tempfile::tempdir().unwrap();
        let token = token_file(dir.path());
        let (base, server) = mock_github(vec![
            (200, "[]"),
            (
                201,
                r#"{"number":43,"title":"Descriptive title","html_url":"https://github.test/pr/43","state":"open"}"#,
            ),
        ])
        .await;
        let pr = create_or_find_with(
            "org/repo",
            "develop",
            "release/1.0+hotfix",
            "Descriptive title",
            "## Summary\nDetailed verification.",
            &base,
            &token,
        )
        .await
        .unwrap();
        let requests = server.await.unwrap();
        assert_eq!(pr.number, 43);
        assert_eq!(requests.len(), 2);
        assert!(requests[1].starts_with("POST /repos/org/repo/pulls "));
        assert!(requests[1].contains("authorization: Bearer brokered-test-token"));
        let body = requests[1].split_once("\r\n\r\n").unwrap().1;
        let payload: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(payload["title"], "Descriptive title");
        assert_eq!(payload["base"], "develop");
        assert_eq!(payload["head"], "release/1.0+hotfix");
        assert_eq!(payload["body"], "## Summary\nDetailed verification.");
    }

    #[tokio::test]
    async fn reports_lookup_and_creation_http_failures() {
        let dir = tempfile::tempdir().unwrap();
        let token = token_file(dir.path());
        for responses in [vec![(403, "{}")], vec![(200, "[]"), (403, "{}")]] {
            let (base, server) = mock_github(responses).await;
            let result = create_or_find_with(
                "org/repo",
                "main",
                "feature/test",
                "Title",
                "Body",
                &base,
                &token,
            )
            .await;
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("HTTP 403"));
            server.await.unwrap();
        }
    }
}
