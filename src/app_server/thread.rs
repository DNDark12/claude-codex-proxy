use serde::{Deserialize, Serialize};

use crate::mapping::approvals::{ApprovalPolicy, SandboxConfig};
use crate::mapping::interaction::UserInteractionBridge;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Active,
    PausedForApproval,
    PausedForClarification,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    Text,
    ToolCall,
    ToolResult,
    ApprovalRequest,
    PlanDelta,
    Error,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeThread {
    pub thread_id: String,
    pub bridge_session_id: String,
    pub cwd: String,
    pub project_root: Option<String>,
    pub approval_policy: ApprovalPolicy,
    pub sandbox_config: SandboxConfig,
    pub created_at_unix: i64,
    pub turn_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeItemRef {
    pub item_id: String,
    pub item_type: ItemType,
    pub surface_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeTurn {
    pub turn_id: String,
    pub thread_id: String,
    pub role: TurnRole,
    pub status: TurnStatus,
    #[serde(default)]
    pub items: Vec<BridgeItemRef>,
    pub pending_interaction: Option<UserInteractionBridge>,
}
