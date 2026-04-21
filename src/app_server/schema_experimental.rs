//! Experimental Codex app-server schema snapshot.
//!
//! Updated against `codex-cli 0.104.0` using:
//! `codex app-server generate-json-schema --experimental --out /tmp/codex-schema-experimental`
//!
//! These types are only available when the client opts into experimental API
//! support during the initialize handshake.

use serde::{Deserialize, Serialize};

pub const GENERATED_WITH_CLI_VERSION: &str = "codex-cli 0.104.0";

// ---- Experimental Capabilities ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentalCapabilities {
    /// Set to true in initialize handshake to opt in
    pub experimental_api: bool,
}

// ---- Thread Compact (may be experimental) ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCompactStartParams {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_token_count: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCompactResult {
    pub thread_id: String,
    pub compacted: bool,
    pub new_token_count: Option<u64>,
}

// ---- Extended Thread Fork (may be experimental) ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadForkParams {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadForkResult {
    pub forked_thread_id: String,
    pub source_thread_id: String,
    pub fork_point_turn_id: Option<String>,
}
