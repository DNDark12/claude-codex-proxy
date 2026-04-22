use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use reqwest::{Client, Response, StatusCode};
use serde_json::Value;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::domain::auth::AuthData;
use crate::domain::codex::CodexResponsesRequest;

const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const CODEX_MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";

const FALLBACK_MODELS: &[&str] = &[
    "gpt-5.4",
    "gpt-5.3-codex",
    "gpt-5.2-codex",
    "gpt-5.2",
    "gpt-5.1-codex-max",
    "gpt-5.1-codex-mini",
];

#[derive(Debug)]
pub enum UpstreamError {
    Transport(anyhow::Error),
    Upstream { status: StatusCode, body: String },
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "transport error: {e}"),
            Self::Upstream { status, body } => write!(f, "upstream error ({status}): {body}"),
        }
    }
}

impl std::error::Error for UpstreamError {}

#[derive(Clone)]
pub struct CodexClient {
    client: Client,
    auth_path: String,
    models_cache: Arc<RwLock<Option<ModelsCache>>>,
}

#[derive(Clone)]
struct ModelsCache {
    expires_at: Instant,
    models: Vec<String>,
}

impl CodexClient {
    pub async fn from_auth_path(auth_path: &str) -> Result<Self> {
        AuthData::load_from_path(auth_path)?;
        let client = Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .no_proxy()
            .build()
            .context("failed to create http client")?;

        Ok(Self {
            client,
            auth_path: auth_path.to_string(),
            models_cache: Arc::new(RwLock::new(None)),
        })
    }

    fn load_auth(&self) -> Result<AuthData> {
        AuthData::load_from_path(&self.auth_path)
    }

    pub async fn create_response(
        &self,
        request: &CodexResponsesRequest,
    ) -> std::result::Result<Response, UpstreamError> {
        let auth = self
            .load_auth()
            .map_err(|error| UpstreamError::Transport(error.context("failed to reload auth")))?;
        let token = auth.bearer_token().ok_or_else(|| {
            UpstreamError::Transport(anyhow::anyhow!("missing bearer token in auth file"))
        })?;

        let mut builder = self
            .client
            .post(CODEX_RESPONSES_URL)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Accept-Encoding", "gzip, deflate, br")
            .header("Referer", "https://chatgpt.com/")
            .header("Origin", "https://chatgpt.com")
            .header("Sec-Fetch-Dest", "empty")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Site", "same-origin")
            .header("Cache-Control", "no-cache")
            .header("Pragma", "no-cache")
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "codex_cli_rs")
            .header("Authorization", format!("Bearer {token}"))
            .header("session_id", Uuid::new_v4().to_string());

        if let Some(account_id) = auth.account_id() {
            builder = builder.header("ChatGPT-Account-Id", account_id);
        }

        let response = builder.json(request).send().await.map_err(|e| {
            UpstreamError::Transport(anyhow::Error::new(e).context("failed to call codex backend"))
        })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(UpstreamError::Upstream { status, body });
        }

        Ok(response)
    }

    pub async fn list_models(&self) -> Vec<String> {
        if let Some(cached) = self.models_cache.read().await.clone() {
            if cached.expires_at > Instant::now() {
                return cached.models;
            }
        }

        let mut models = match self.fetch_models_from_backend().await {
            Ok(m) if !m.is_empty() => m,
            Ok(_) | Err(_) => FALLBACK_MODELS.iter().map(|v| (*v).to_string()).collect(),
        };

        models.sort();
        models.dedup();

        *self.models_cache.write().await = Some(ModelsCache {
            expires_at: Instant::now() + Duration::from_secs(300),
            models: models.clone(),
        });

        models
    }

    async fn fetch_models_from_backend(&self) -> Result<Vec<String>> {
        let auth = self
            .load_auth()
            .context("failed to reload auth before fetching models")?;
        let token = auth
            .bearer_token()
            .ok_or_else(|| anyhow::anyhow!("missing bearer token in auth file"))?;

        let mut builder = self
            .client
            .get(CODEX_MODELS_URL)
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {token}"))
            .header("OpenAI-Beta", "responses=experimental");

        if let Some(account_id) = auth.account_id() {
            builder = builder.header("ChatGPT-Account-Id", account_id);
        }

        let response = builder.send().await.context("failed to fetch models")?;
        if !response.status().is_success() {
            anyhow::bail!("models endpoint failed with {}", response.status());
        }

        let payload: Value = response
            .json()
            .await
            .context("failed to parse models response")?;

        Ok(extract_models(&payload))
    }

    pub fn is_tool_unsupported(body: &str) -> bool {
        let lower = body.to_lowercase();
        (lower.contains("tool") || lower.contains("function"))
            && (lower.contains("unsupported")
                || lower.contains("invalid")
                || lower.contains("not allowed")
                || lower.contains("not supported"))
    }
}

fn extract_models(payload: &Value) -> Vec<String> {
    let mut out = Vec::new();

    fn collect(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Array(arr) => {
                for v in arr {
                    collect(v, out);
                }
            }
            Value::Object(map) => {
                if let Some(v) = map.get("id").and_then(Value::as_str) {
                    out.push(v.to_string());
                }
                if let Some(v) = map.get("slug").and_then(Value::as_str) {
                    out.push(v.to_string());
                }
                if let Some(v) = map.get("model_slug").and_then(Value::as_str) {
                    out.push(v.to_string());
                }
                if let Some(v) = map.get("name").and_then(Value::as_str) {
                    if v.starts_with("gpt-") {
                        out.push(v.to_string());
                    }
                }

                for key in ["models", "data", "categories", "chat_models", "items"] {
                    if let Some(child) = map.get(key) {
                        collect(child, out);
                    }
                }
            }
            _ => {}
        }
    }

    collect(payload, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_client_reloads_auth_file_contents() {
        let dir = std::env::temp_dir().join(format!("codex-client-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let auth_path = dir.join("auth.json");
        std::fs::write(
            &auth_path,
            r#"{"tokens":{"access_token":"token-a","account_id":"acc-a","refresh_token":"rt-a"}}"#,
        )
        .unwrap();

        let client = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(CodexClient::from_auth_path(auth_path.to_str().unwrap()))
            .unwrap();
        std::fs::write(
            &auth_path,
            r#"{"tokens":{"access_token":"token-b","account_id":"acc-b","refresh_token":"rt-b"}}"#,
        )
        .unwrap();

        let auth = client.load_auth().unwrap();
        assert_eq!(auth.bearer_token(), Some("token-b"));
        assert_eq!(auth.account_id(), Some("acc-b"));

        let _ = std::fs::remove_dir_all(dir);
    }
}
