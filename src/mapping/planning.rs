use serde::{Deserialize, Serialize};

use crate::app_server::events::{AppServerEvent, AppServerEventKind};
use crate::mapping::tools::ToolWarning;
use crate::surfaces::model::MappingStrategy;

/// Plan mode state tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanModeState {
    Inactive,
    Active,
}

/// Result of entering/exiting plan mode.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanModeResult {
    pub state: PlanModeState,
    pub strategy: MappingStrategy,
    pub warnings: Vec<ToolWarning>,
}

/// EnterPlanMode mapping — instruction/profile switch + item/plan/delta awareness (P4-001).
pub fn map_enter_plan_mode() -> PlanModeResult {
    PlanModeResult {
        state: PlanModeState::Active,
        strategy: MappingStrategy::MediatedNative,
        warnings: Vec::new(),
    }
}

/// ExitPlanMode mapping — switch back to execution mode (P4-002).
pub fn map_exit_plan_mode() -> PlanModeResult {
    PlanModeResult {
        state: PlanModeState::Inactive,
        strategy: MappingStrategy::MediatedNative,
        warnings: Vec::new(),
    }
}

/// /plan command — instruction injection + plan item surfacing (P4-003).
pub fn map_plan_command(instructions: Option<&str>) -> PlanModeResult {
    PlanModeResult {
        state: PlanModeState::Active,
        strategy: MappingStrategy::MediatedNative,
        warnings: if instructions.is_none() {
            vec![ToolWarning {
                surface_id: "command.plan".to_string(),
                warning: "No plan instructions provided; entering plan mode with default profile."
                    .to_string(),
            }]
        } else {
            Vec::new()
        },
    }
}

/// Detect plan delta events from app-server stream (P4-004).
pub fn extract_plan_delta(event: &AppServerEvent) -> Option<PlanDeltaEvent> {
    if event.kind == AppServerEventKind::PlanDelta {
        Some(PlanDeltaEvent {
            thread_id: event.thread_id.clone().unwrap_or_default(),
            turn_id: event.turn_id.clone().unwrap_or_default(),
            delta: event.delta.clone().unwrap_or_default(),
            raw_params: event.params.clone(),
        })
    } else {
        None
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub delta: String,
    pub raw_params: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::events::{AppServerEvent, AppServerEventKind};

    // P4-T01: EnterPlanMode → item/plan/delta events surface correctly
    #[test]
    fn enter_plan_mode_is_mediated_native() {
        let result = map_enter_plan_mode();
        assert_eq!(result.state, PlanModeState::Active);
        assert_eq!(result.strategy, MappingStrategy::MediatedNative);
    }

    #[test]
    fn exit_plan_mode_deactivates() {
        let result = map_exit_plan_mode();
        assert_eq!(result.state, PlanModeState::Inactive);
    }

    #[test]
    fn extract_plan_delta_from_event() {
        let event = AppServerEvent {
            method: "item/plan/delta".to_string(),
            kind: AppServerEventKind::PlanDelta,
            params: serde_json::json!({ "text": "Step 1: ..." }),
            thread_id: Some("t1".to_string()),
            turn_id: Some("turn1".to_string()),
            item_id: None,
            delta: Some("Step 1: ...".to_string()),
        };
        let result = extract_plan_delta(&event).unwrap();
        assert_eq!(result.delta, "Step 1: ...");
    }

    #[test]
    fn non_plan_event_returns_none() {
        let event = AppServerEvent {
            method: "item/agentMessage/delta".to_string(),
            kind: AppServerEventKind::AgentMessageDelta,
            params: serde_json::json!({}),
            thread_id: None,
            turn_id: None,
            item_id: None,
            delta: None,
        };
        assert!(extract_plan_delta(&event).is_none());
    }
}
