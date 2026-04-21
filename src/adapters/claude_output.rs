use serde::Serialize;
use serde_json::Value;

use crate::app_server::events::{AppServerEvent, AppServerEventKind};
use crate::surfaces::model::{MappingDecision, MappingStrategy, OperationMode, SurfaceBucket, SurfaceDescriptor};
use crate::app_server::session::ApiStability;

/// Bridge metadata attached to every response (P7-009).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeMetadata {
    pub surface_id: String,
    pub strategy: MappingStrategy,
    pub target_backend: String,
    pub operation_mode: String,
    pub api_stability: String,
    pub downgraded: bool,
    pub tier: u8,
    pub bucket: SurfaceBucket,
    pub warnings: Vec<String>,
}

impl BridgeMetadata {
    pub fn native(surface_id: &str, tier: u8, bucket: SurfaceBucket) -> Self {
        Self {
            surface_id: surface_id.to_string(),
            strategy: MappingStrategy::Native,
            target_backend: "codex_app_server".to_string(),
            operation_mode: "auto-hybrid".to_string(),
            api_stability: "stable".to_string(),
            downgraded: false,
            tier,
            bucket,
            warnings: Vec::new(),
        }
    }

    pub fn from_decision(
        surface: &SurfaceDescriptor,
        decision: &MappingDecision,
        operation_mode: OperationMode,
        api_stability: ApiStability,
    ) -> Self {
        Self {
            surface_id: surface.id.clone(),
            strategy: decision.strategy,
            target_backend: decision.target_backend.clone(),
            operation_mode: serde_json::to_string(&operation_mode)
                .unwrap_or_else(|_| "\"auto-hybrid\"".to_string())
                .trim_matches('"')
                .to_string(),
            api_stability: serde_json::to_string(&api_stability)
                .unwrap_or_else(|_| "\"stable\"".to_string())
                .trim_matches('"')
                .to_string(),
            downgraded: !matches!(
                decision.strategy,
                MappingStrategy::Native | MappingStrategy::MediatedNative
            ),
            tier: surface.tier,
            bucket: surface.bucket,
            warnings: decision.warnings.clone(),
        }
    }
}

/// Translate app-server events to Claude-compatible output format (P4-004, P7-009).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeOutputEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge: Option<BridgeMetadata>,
}

/// Convert app-server event stream to Claude-compatible events.
pub fn translate_event(event: &AppServerEvent) -> ClaudeOutputEvent {
    match event.kind {
        AppServerEventKind::AgentMessageDelta => ClaudeOutputEvent {
            event_type: "content_block_delta".to_string(),
            data: serde_json::json!({
                "type": "text_delta",
                "text": event.delta.as_deref().unwrap_or(""),
            }),
            bridge: None,
        },
        AppServerEventKind::PlanDelta => ClaudeOutputEvent {
            event_type: "plan_delta".to_string(),
            data: serde_json::json!({
                "type": "plan_delta",
                "delta": event.delta.as_deref().unwrap_or(""),
                "params": event.params,
            }),
            bridge: None,
        },
        AppServerEventKind::TurnCompleted => ClaudeOutputEvent {
            event_type: "message_stop".to_string(),
            data: serde_json::json!({ "type": "message_stop" }),
            bridge: None,
        },
        AppServerEventKind::Error => ClaudeOutputEvent {
            event_type: "error".to_string(),
            data: event.params.clone(),
            bridge: None,
        },
        _ => ClaudeOutputEvent {
            event_type: format!("bridge_{}", event.method),
            data: event.params.clone(),
            bridge: None,
        },
    }
}
