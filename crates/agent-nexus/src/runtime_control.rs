//! Typed client for the Manager runtime API.
//!
//! A hosted controller authenticates with a short-lived workspace runtime
//! credential.  It never receives a Coder user or operator credential.
//! Manager derives the organization, tenant, repository and workspace scope
//! from this credential on every request.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct RuntimeControlClient {
    base_url: String,
    http: reqwest::Client,
    tokens: Arc<Mutex<RuntimeTokens>>,
}

#[derive(Clone)]
struct RuntimeTokens {
    access: String,
    refresh: String,
}

#[derive(Debug, Deserialize)]
struct RefreshResponse {
    access_token: String,
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct GithubCredentialResponse {
    token: String,
}

#[derive(Debug, Deserialize)]
pub struct WorkerRequestResponse {
    pub workspace_id: String,
    pub operation_id: String,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct WorkerStatus {
    pub workspace_id: String,
    pub role: String,
    pub slot: Option<i32>,
    pub desired_state: String,
    pub observed_state: String,
}

#[derive(Debug, Deserialize)]
pub struct RuntimeChatResponse {
    pub chat_id: String,
    pub workspace_id: String,
}

#[derive(Serialize)]
struct WorkerRequest<'a> {
    role: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ticket_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    a2a_pair_token: Option<&'a str>,
}

impl RuntimeControlClient {
    /// Build a client from the runtime registration result exported by the
    /// Nexus template.  These values exist only in the workspace process.
    pub fn from_env() -> Result<Option<Self>> {
        let base_url = match config::EnvConfig::runtime_manager_url() {
            Some(value) => value,
            None => return Ok(None),
        };
        let access = std::env::var("OF_RUNTIME_ACCESS_TOKEN")
            .context("OPENFLOWS_MANAGER_URL is set but OF_RUNTIME_ACCESS_TOKEN is missing")?;
        let refresh = std::env::var("OF_RUNTIME_REFRESH_TOKEN")
            .context("OPENFLOWS_MANAGER_URL is set but OF_RUNTIME_REFRESH_TOKEN is missing")?;
        if access.trim().is_empty() || refresh.trim().is_empty() {
            bail!("runtime credential values must not be empty");
        }
        Ok(Some(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            tokens: Arc::new(Mutex::new(RuntimeTokens { access, refresh })),
        }))
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<(reqwest::RequestBuilder, String)> {
        let token = self.tokens.lock().await.access.clone();
        Ok((
            self.http
                .request(method, format!("{}{}", self.base_url, path))
                .bearer_auth(&token),
            token,
        ))
    }

    async fn refresh_if_needed(&self, rejected_access: &str) -> Result<()> {
        // Keep the lock during refresh so a heartbeat and a broker request do
        // not consume the same single-use refresh token concurrently.
        let mut tokens = self.tokens.lock().await;
        if tokens.access != rejected_access {
            return Ok(());
        }
        let response = self
            .http
            .post(format!("{}/api/v1/runtime/refresh", self.base_url))
            .bearer_auth(&tokens.refresh)
            .send()
            .await
            .context("failed to refresh runtime credential")?;
        if !response.status().is_success() {
            bail!(
                "runtime credential refresh failed with {}",
                response.status()
            );
        }
        let refreshed: RefreshResponse = response
            .json()
            .await
            .context("invalid runtime refresh response")?;
        tokens.access = refreshed.access_token;
        tokens.refresh = refreshed.refresh_token;
        Ok(())
    }

    async fn send_retrying_refresh(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
        idempotency_key: Option<&str>,
    ) -> Result<reqwest::Response> {
        for attempt in 0..2 {
            let (mut request, access) = self.request(method.clone(), path).await?;
            if let Some(value) = body.clone() {
                request = request.json(&value);
            }
            if let Some(key) = idempotency_key {
                request = request.header("Idempotency-Key", key);
            }
            let response = request.send().await.context("runtime API request failed")?;
            if response.status() != reqwest::StatusCode::UNAUTHORIZED || attempt == 1 {
                return Ok(response);
            }
            self.refresh_if_needed(&access).await?;
        }
        unreachable!()
    }

    pub async fn heartbeat(&self) -> Result<()> {
        let response = self
            .send_retrying_refresh(
                reqwest::Method::POST,
                "/api/v1/runtime/heartbeat",
                None,
                None,
            )
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            let code = body
                .pointer("/error/code")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("NON_API_RESPONSE");
            let request_id = body
                .pointer("/error/request_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            bail!("runtime heartbeat failed with {status} ({code}, request_id={request_id})")
        }
    }

    /// Obtain a repository-scoped installation token for Nexus REST calls.
    /// Manager derives the installation, repository, and permissions from the
    /// authenticated workspace; the controller supplies only the purpose.
    pub async fn github_api_token(&self) -> Result<String> {
        let response = self
            .send_retrying_refresh(
                reqwest::Method::POST,
                "/api/v1/runtime/github-credentials",
                Some(serde_json::json!({ "purpose": "api" })),
                None,
            )
            .await?;
        if !response.status().is_success() {
            bail!("GitHub credential broker returned {}", response.status());
        }
        let credential: GithubCredentialResponse = response
            .json()
            .await
            .context("invalid GitHub credential broker response")?;
        if credential.token.is_empty() {
            bail!("GitHub credential broker returned an empty token");
        }
        Ok(credential.token)
    }

    pub async fn request_worker(
        &self,
        role: &str,
        slot: Option<i32>,
        ticket_id: Option<&str>,
        branch: Option<&str>,
        a2a_pair_token: Option<&str>,
        idempotency_key: &str,
    ) -> Result<WorkerRequestResponse> {
        let body = serde_json::to_value(WorkerRequest {
            role,
            slot,
            ticket_id,
            branch,
            a2a_pair_token,
        })?;
        let response = self
            .send_retrying_refresh(
                reqwest::Method::POST,
                "/api/v1/runtime/workspaces",
                Some(body),
                Some(idempotency_key),
            )
            .await?;
        if response.status().is_success() {
            response
                .json()
                .await
                .context("invalid worker request response")
        } else {
            bail!("worker request failed with {}", response.status())
        }
    }

    pub async fn worker_status(&self, workspace_id: &str) -> Result<WorkerStatus> {
        let response = self
            .send_retrying_refresh(
                reqwest::Method::GET,
                &format!("/api/v1/runtime/workspaces/{workspace_id}"),
                None,
                None,
            )
            .await?;
        if response.status().is_success() {
            response
                .json()
                .await
                .context("invalid worker status response")
        } else {
            bail!("worker status request failed with {}", response.status())
        }
    }

    pub async fn create_chat(
        &self,
        workspace_id: &str,
        ticket_id: &str,
        prompt: &str,
    ) -> Result<RuntimeChatResponse> {
        let response = self.send_retrying_refresh(
            reqwest::Method::POST,
            "/api/v1/runtime/chats",
            Some(serde_json::json!({"workspace_id":workspace_id,"ticket_id":ticket_id,"prompt":prompt})),
            None,
        ).await?;
        if !response.status().is_success() {
            let status = response.status();
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            let code = body
                .pointer("/error/code")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("CODER_CHAT_UNAVAILABLE");
            bail!("managed chat creation failed with {status} ({code})");
        }
        response
            .json()
            .await
            .context("invalid runtime chat response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::{Matcher, Server};

    #[tokio::test]
    async fn managed_chat_uses_runtime_credential_and_scoped_request() {
        let mut server = Server::new_async().await;
        let request = server
            .mock("POST", "/api/v1/runtime/chats")
            .match_header("authorization", "Bearer runtime-access")
            .match_body(Matcher::Json(serde_json::json!({
                "workspace_id":"worker-id", "ticket_id":"T-CI-001", "prompt":"Begin work"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"chat_id":"chat-id","workspace_id":"worker-id"}"#)
            .create_async()
            .await;
        let runtime = RuntimeControlClient {
            base_url: server.url(),
            http: reqwest::Client::new(),
            tokens: Arc::new(Mutex::new(RuntimeTokens {
                access: "runtime-access".into(),
                refresh: "runtime-refresh".into(),
            })),
        };
        let chat = runtime
            .create_chat("worker-id", "T-CI-001", "Begin work")
            .await
            .unwrap();
        assert_eq!(chat.chat_id, "chat-id");
        request.assert_async().await;
    }

    #[tokio::test]
    async fn broker_api_token_uses_runtime_auth_and_api_purpose() {
        let mut server = Server::new_async().await;
        let exchange = server
            .mock("POST", "/api/v1/runtime/github-credentials")
            .match_header("authorization", "Bearer runtime-access")
            .match_body(Matcher::JsonString(r#"{"purpose":"api"}"#.into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"token":"installation-token"}"#)
            .create_async()
            .await;
        let client = RuntimeControlClient {
            base_url: server.url(),
            http: reqwest::Client::new(),
            tokens: Arc::new(Mutex::new(RuntimeTokens {
                access: "runtime-access".into(),
                refresh: "runtime-refresh".into(),
            })),
        };

        assert_eq!(
            client.github_api_token().await.unwrap(),
            "installation-token"
        );
        exchange.assert_async().await;
    }

    #[tokio::test]
    async fn broker_and_heartbeat_share_rotated_runtime_session() {
        let mut server = Server::new_async().await;
        let expired = server
            .mock("POST", "/api/v1/runtime/github-credentials")
            .match_header("authorization", "Bearer expired-access")
            .with_status(401)
            .expect(1)
            .create_async()
            .await;
        let refresh = server
            .mock("POST", "/api/v1/runtime/refresh")
            .match_header("authorization", "Bearer runtime-refresh")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"new-access","refresh_token":"new-refresh"}"#)
            .expect(1)
            .create_async()
            .await;
        let exchange = server
            .mock("POST", "/api/v1/runtime/github-credentials")
            .match_header("authorization", "Bearer new-access")
            .match_body(Matcher::JsonString(r#"{"purpose":"api"}"#.into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"token":"installation-token"}"#)
            .expect(1)
            .create_async()
            .await;
        let heartbeat = server
            .mock("POST", "/api/v1/runtime/heartbeat")
            .match_header("authorization", "Bearer new-access")
            .with_status(200)
            .expect(1)
            .create_async()
            .await;
        let client = RuntimeControlClient {
            base_url: server.url(),
            http: reqwest::Client::new(),
            tokens: Arc::new(Mutex::new(RuntimeTokens {
                access: "expired-access".into(),
                refresh: "runtime-refresh".into(),
            })),
        };
        let heartbeat_client = client.clone();

        assert_eq!(
            client.github_api_token().await.unwrap(),
            "installation-token"
        );
        heartbeat_client.heartbeat().await.unwrap();
        expired.assert_async().await;
        refresh.assert_async().await;
        exchange.assert_async().await;
        heartbeat.assert_async().await;
    }
}
