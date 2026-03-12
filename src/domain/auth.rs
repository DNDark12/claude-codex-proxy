use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AuthData {
    #[serde(rename = "OPENAI_API_KEY")]
    pub api_key: Option<String>,
    pub access_token: Option<String>,
    pub account_id: Option<String>,
    pub tokens: Option<TokenData>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenData {
    pub access_token: String,
    pub account_id: String,
    pub refresh_token: Option<String>,
}

impl AuthData {
    pub fn bearer_token(&self) -> Option<&str> {
        self.tokens
            .as_ref()
            .map(|v| v.access_token.as_str())
            .or_else(|| self.access_token.as_deref())
            .or_else(|| self.api_key.as_deref())
    }

    pub fn account_id(&self) -> Option<&str> {
        self.tokens
            .as_ref()
            .map(|v| v.account_id.as_str())
            .or(self.account_id.as_deref())
    }

    pub fn load_from_path(auth_path: &str) -> Result<Self> {
        let expanded = if auth_path.starts_with("~/") {
            let home = std::env::var("HOME").context("HOME environment variable not set")?;
            auth_path.replacen('~', &home, 1)
        } else {
            auth_path.to_string()
        };

        let content = std::fs::read_to_string(&expanded)
            .with_context(|| format!("Failed to read auth file at {}", expanded))?;

        let auth = serde_json::from_str::<Self>(&content)
            .with_context(|| format!("Failed to parse auth file {}", expanded))?;

        Ok(auth)
    }
}
