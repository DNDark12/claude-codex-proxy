//! Stable Codex app-server schema snapshot.
//!
//! Updated against `codex-cli 0.104.0` using:
//! `codex app-server generate-json-schema --out /tmp/codex-schema-stable`
//!
//! These are the Rust-side representations currently used by the bridge for the
//! stable v2 thread/turn flow. Regenerate whenever the pinned Codex CLI changes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const GENERATED_WITH_CLI_VERSION: &str = "codex-cli 0.104.0";

// ---- Handshake ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_name: String,
    pub client_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ClientCapabilities>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental_api: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub server_name: String,
    pub server_version: String,
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    #[serde(default)]
    pub supported_methods: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<Value>,
}

// ---- Config Requirements ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigRequirementsResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_approval_policies: Option<Vec<AskForApproval>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_sandbox_modes: Option<Vec<SandboxMode>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_web_search_modes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enforce_residency: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AskForApproval {
    Untrusted,
    OnFailure,
    OnRequest,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ReadOnlyAccess {
    Restricted {
        #[serde(default = "default_true")]
        include_platform_defaults: bool,
        #[serde(default)]
        readable_roots: Vec<String>,
    },
    FullAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SandboxPolicy {
    DangerFullAccess,
    ReadOnly {
        #[serde(default)]
        access: Option<ReadOnlyAccess>,
    },
    ExternalSandbox {
        #[serde(default)]
        network_access: Option<String>,
    },
    WorkspaceWrite {
        #[serde(default)]
        network_access: Option<bool>,
        #[serde(default)]
        writable_roots: Vec<String>,
        #[serde(default)]
        exclude_slash_tmp: Option<bool>,
        #[serde(default)]
        exclude_tmpdir_env_var: Option<bool>,
        #[serde(default)]
        read_only_access: Option<ReadOnlyAccess>,
    },
}

// ---- Thread ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ephemeral: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartResult {
    pub thread: ThreadDescriptor,
    pub cwd: String,
    pub approval_policy: AskForApproval,
    pub sandbox: SandboxPolicy,
    pub model: String,
    pub model_provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDescriptor {
    pub id: String,
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub turns: Vec<TurnDescriptor>,
}

// ---- Turn ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserInput {
    Text {
        text: String,
        #[serde(default)]
        text_elements: Vec<Value>,
    },
    Image {
        url: String,
    },
    LocalImage {
        path: String,
    },
    Skill {
        name: String,
        path: String,
    },
    Mention {
        name: String,
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub thread_id: String,
    pub input: Vec<UserInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<SandboxPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartResult {
    pub turn: TurnDescriptor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnDescriptor {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub items: Vec<Value>,
}

// ---- Model ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResult {
    pub data: Vec<ModelDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDescriptor {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub description: String,
    pub hidden: bool,
    pub is_default: bool,
}

// ---- Command ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecParams {
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

fn default_true() -> bool {
    true
}
