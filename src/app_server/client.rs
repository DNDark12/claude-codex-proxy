use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use super::events::AppServerEvent;
use super::handshake::{perform_handshake, HandshakeState};
use super::jsonrpc::{JsonRpcNotification, JsonRpcRequest, RequestId};
use super::session::ApiStability;
use super::transport_stdio::{StdioTransport, StdioTransportOptions};
use crate::mapping::approvals::{ApprovalPolicy, ConfigRequirements, SandboxConfig};

#[derive(Debug, Clone)]
pub struct AppServerConnectOptions {
    pub binary_path: String,
    pub current_dir: Option<PathBuf>,
    pub extra_env: Vec<(String, String)>,
    pub api_stability: ApiStability,
}

impl Default for AppServerConnectOptions {
    fn default() -> Self {
        Self {
            binary_path: std::env::var("CLAUDE_CODEX_PROXY_CODEX_BIN")
                .unwrap_or_else(|_| "codex".to_string()),
            current_dir: None,
            extra_env: Vec::new(),
            api_stability: ApiStability::Stable,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppServerClient {
    transport: Arc<StdioTransport>,
    handshake: HandshakeState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppServerModel {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub description: String,
    pub hidden: bool,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub auth_method: Option<String>,
    pub requires_openai_auth: Option<bool>,
    pub account: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartRequest {
    pub cwd: Option<String>,
    pub approval_policy: Option<ApprovalPolicy>,
    pub sandbox: Option<SandboxConfig>,
    pub model: Option<String>,
    pub model_provider: Option<String>,
    pub developer_instructions: Option<String>,
    pub base_instructions: Option<String>,
    pub ephemeral: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartResult {
    pub thread_id: String,
    pub cwd: String,
    pub approval_policy: ApprovalPolicy,
    pub sandbox: Value,
    pub model: String,
    pub model_provider: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserInput {
    Text { text: String },
    Image { url: String },
    LocalImage { path: String },
    Skill { name: String, path: String },
    Mention { name: String, path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartRequest {
    pub thread_id: String,
    pub input: Vec<UserInput>,
    pub approval_policy: Option<ApprovalPolicy>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub sandbox_policy: Option<Value>,
    pub effort: Option<String>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartResult {
    pub turn_id: String,
    pub status: String,
    pub items: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct ModelListResponse {
    data: Vec<AppServerModel>,
}

#[derive(Debug, Deserialize)]
struct GetAuthStatusResponse {
    #[serde(rename = "authMethod")]
    auth_method: Option<String>,
    #[serde(rename = "requiresOpenaiAuth")]
    requires_openai_auth: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AccountReadResponse {
    account: Option<Value>,
    #[serde(rename = "requiresOpenaiAuth")]
    requires_openai_auth: bool,
}

#[derive(Debug, Deserialize)]
struct ThreadStartRawResponse {
    thread: ThreadDescriptor,
    cwd: String,
    #[serde(rename = "approvalPolicy")]
    approval_policy: ApprovalPolicy,
    sandbox: Value,
    model: String,
    #[serde(rename = "modelProvider")]
    model_provider: String,
}

#[derive(Debug, Deserialize)]
struct ThreadDescriptor {
    id: String,
    #[serde(rename = "createdAt")]
    created_at: i64,
}

#[derive(Debug, Deserialize)]
struct TurnStartRawResponse {
    turn: TurnDescriptor,
}

#[derive(Debug, Deserialize)]
struct TurnDescriptor {
    id: String,
    status: String,
    #[serde(default)]
    items: Vec<Value>,
}

impl AppServerClient {
    pub async fn connect(options: AppServerConnectOptions) -> Result<Self> {
        let transport = Arc::new(
            StdioTransport::spawn(StdioTransportOptions {
                binary_path: options.binary_path,
                current_dir: options.current_dir,
                extra_env: options.extra_env,
            })
            .await?,
        );
        let handshake = perform_handshake(&transport, options.api_stability).await?;
        Ok(Self {
            transport,
            handshake,
        })
    }

    pub fn handshake(&self) -> &HandshakeState {
        &self.handshake
    }

    pub fn config_requirements(&self) -> Option<&ConfigRequirements> {
        self.handshake.requirements.as_ref()
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<JsonRpcNotification> {
        self.transport.subscribe_notifications()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<JsonRpcNotification> {
        self.subscribe_notifications()
    }

    pub fn subscribe_server_requests(&self) -> broadcast::Receiver<JsonRpcRequest> {
        self.transport.subscribe_server_requests()
    }

    pub async fn model_list(&self) -> Result<Vec<AppServerModel>> {
        let response: ModelListResponse = self.transport.request("model/list", json!({})).await?;
        Ok(response.data)
    }

    pub async fn command_exec(
        &self,
        command: Vec<String>,
        cwd: Option<String>,
        timeout_ms: Option<i64>,
    ) -> Result<CommandExecResult> {
        self.transport
            .request(
                "command/exec",
                json!({
                    "command": command,
                    "cwd": cwd,
                    "timeoutMs": timeout_ms,
                }),
            )
            .await
    }

    pub async fn auth_status(&self) -> Result<AuthStatus> {
        let status: GetAuthStatusResponse =
            self.transport.request("getAuthStatus", json!({})).await?;
        let account: AccountReadResponse =
            self.transport.request("account/read", json!({})).await?;
        Ok(AuthStatus {
            auth_method: status.auth_method,
            requires_openai_auth: Some(
                account.requires_openai_auth || status.requires_openai_auth.unwrap_or(false),
            ),
            account: account.account,
        })
    }

    pub async fn thread_start(&self, request: ThreadStartRequest) -> Result<ThreadStartResult> {
        let response: ThreadStartRawResponse = self
            .transport
            .request(
                "thread/start",
                json!({
                    "cwd": request.cwd,
                    "approvalPolicy": request.approval_policy,
                    "sandbox": request.sandbox,
                    "model": request.model,
                    "modelProvider": request.model_provider,
                    "developerInstructions": request.developer_instructions,
                    "baseInstructions": request.base_instructions,
                    "ephemeral": request.ephemeral,
                }),
            )
            .await?;

        Ok(ThreadStartResult {
            thread_id: response.thread.id,
            cwd: response.cwd,
            approval_policy: response.approval_policy,
            sandbox: response.sandbox,
            model: response.model,
            model_provider: response.model_provider,
            created_at: response.thread.created_at,
        })
    }

    pub async fn turn_start(&self, request: TurnStartRequest) -> Result<TurnStartResult> {
        let response: TurnStartRawResponse = self
            .transport
            .request(
                "turn/start",
                json!({
                    "threadId": request.thread_id,
                    "input": request.input,
                    "approvalPolicy": request.approval_policy,
                    "cwd": request.cwd,
                    "model": request.model,
                    "sandboxPolicy": request.sandbox_policy,
                    "effort": request.effort,
                    "summary": request.summary,
                }),
            )
            .await?;

        Ok(TurnStartResult {
            turn_id: response.turn.id,
            status: response.turn.status,
            items: response.turn.items,
        })
    }

    pub async fn turn_interrupt(&self, thread_id: &str, turn_id: &str) -> Result<Value> {
        self.transport
            .request(
                "turn/interrupt",
                json!({
                    "threadId": thread_id,
                    "turnId": turn_id,
                }),
            )
            .await
    }

    pub async fn thread_resume(&self, thread_id: &str) -> Result<Value> {
        self.transport
            .request("thread/resume", json!({ "threadId": thread_id }))
            .await
    }

    pub async fn thread_rollback(&self, thread_id: &str, turn_id: Option<&str>) -> Result<Value> {
        self.transport
            .request(
                "thread/rollback",
                json!({
                    "threadId": thread_id,
                    "turnId": turn_id,
                }),
            )
            .await
    }

    pub async fn thread_fork(&self, thread_id: &str, turn_id: Option<&str>) -> Result<Value> {
        self.transport
            .request(
                "thread/fork",
                json!({
                    "threadId": thread_id,
                    "turnId": turn_id,
                }),
            )
            .await
    }

    pub async fn respond_to_server_request(&self, id: RequestId, result: Value) -> Result<()> {
        self.transport.respond_success(id, result).await
    }

    pub async fn kill(&self) -> Result<()> {
        self.transport.kill().await
    }

    pub async fn collect_text_deltas(
        &self,
        thread_id: &str,
        turn_id: &str,
        mut notifications: broadcast::Receiver<JsonRpcNotification>,
    ) -> Result<Vec<AppServerEvent>> {
        let mut out = Vec::new();
        while let Ok(notification) = notifications.recv().await {
            let event = AppServerEvent::from(notification);
            if event.thread_id.as_deref() == Some(thread_id)
                && event.turn_id.as_deref() == Some(turn_id)
            {
                let done = matches!(event.kind, super::events::AppServerEventKind::TurnCompleted);
                out.push(event);
                if done {
                    break;
                }
            }
        }
        Ok(out)
    }
}
