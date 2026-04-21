use serde::{Deserialize, Serialize};

use super::thread::BridgeThread;
use crate::surfaces::model::{OperationMode, SurfaceFamily};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Stdio,
    Websocket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiStability {
    Stable,
    Experimental,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DelegationPolicy {
    Never,
    ExplicitOnly,
    Heuristic,
    ForceForSurface(Vec<SurfaceFamily>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeSession {
    pub bridge_session_id: String,
    pub claude_session_id: Option<String>,
    pub thread: BridgeThread,
    pub transport: TransportKind,
    pub operation_mode: OperationMode,
    pub api_stability: ApiStability,
    pub delegation_policy: DelegationPolicy,
    #[serde(default)]
    pub active_guidance_layers: Vec<String>,
    #[serde(default)]
    pub active_skills: Vec<String>,
    #[serde(default)]
    pub active_jobs: Vec<String>,
    pub state_version: u64,
}
