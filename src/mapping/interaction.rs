use serde::{Deserialize, Serialize};

use crate::app_server::events::{AppServerEvent, AppServerEventKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserInteractionKind {
    ClarificationQuestion {
        question: String,
        context: Option<String>,
    },
    ApprovalRequest {
        action_description: String,
        approval_policy: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionStatus {
    Pending,
    Answered,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInteractionBridge {
    pub interaction_id: String,
    pub kind: UserInteractionKind,
    pub turn_id: String,
    pub surface_id: String,
    pub status: InteractionStatus,
}

/// Detect if an app-server event signals a clarification pause (P1-032).
pub fn detect_clarification_pause(event: &AppServerEvent) -> Option<ClarificationPauseInfo> {
    if event.kind == AppServerEventKind::TerminalInteraction {
        if let Some(action) = event.params.get("action").and_then(|v| v.as_str()) {
            if action == "ask_user" || action == "askUser" || action == "clarification" {
                let question = event
                    .params
                    .get("question")
                    .or_else(|| event.params.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Please provide more information")
                    .to_string();
                let context = event
                    .params
                    .get("context")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                return Some(ClarificationPauseInfo {
                    thread_id: event.thread_id.clone().unwrap_or_default(),
                    turn_id: event.turn_id.clone().unwrap_or_default(),
                    question,
                    context,
                });
            }
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClarificationPauseInfo {
    pub thread_id: String,
    pub turn_id: String,
    pub question: String,
    pub context: Option<String>,
}

/// Classify an event as either clarification or approval (never both).
pub fn classify_interaction(event: &AppServerEvent) -> Option<InteractionClassification> {
    if let Some(clarification) = detect_clarification_pause(event) {
        return Some(InteractionClassification::Clarification(clarification));
    }
    if let Some(approval) = crate::mapping::approvals::detect_approval_pause(event) {
        return Some(InteractionClassification::Approval(approval));
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionClassification {
    Clarification(ClarificationPauseInfo),
    Approval(crate::mapping::approvals::ApprovalPauseInfo),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::events::{AppServerEvent, AppServerEventKind};

    fn ask_user_event() -> AppServerEvent {
        AppServerEvent {
            method: "terminal_interaction".to_string(),
            kind: AppServerEventKind::TerminalInteraction,
            params: serde_json::json!({ "action": "ask_user", "question": "Which file?", "context": "multiple matches" }),
            thread_id: Some("t1".to_string()),
            turn_id: Some("turn1".to_string()),
            item_id: None,
            delta: None,
        }
    }

    fn approval_event() -> AppServerEvent {
        AppServerEvent {
            method: "terminal_interaction".to_string(),
            kind: AppServerEventKind::TerminalInteraction,
            params: serde_json::json!({ "action": "approval_request", "description": "Write file" }),
            thread_id: Some("t1".to_string()),
            turn_id: Some("turn1".to_string()),
            item_id: None,
            delta: None,
        }
    }

    // P3-T06: AskUserQuestion dispatched as clarification, not approval
    #[test]
    fn ask_user_is_clarification_not_approval() {
        let result = classify_interaction(&ask_user_event());
        assert!(matches!(result, Some(InteractionClassification::Clarification(_))));
    }

    #[test]
    fn approval_event_is_approval_not_clarification() {
        let result = classify_interaction(&approval_event());
        assert!(matches!(result, Some(InteractionClassification::Approval(_))));
    }

    #[test]
    fn clarification_extracts_question() {
        let pause = detect_clarification_pause(&ask_user_event()).unwrap();
        assert_eq!(pause.question, "Which file?");
        assert_eq!(pause.context.as_deref(), Some("multiple matches"));
    }

    #[test]
    fn non_interaction_event_returns_none() {
        let event = AppServerEvent {
            method: "item/agentMessage/delta".to_string(),
            kind: AppServerEventKind::AgentMessageDelta,
            params: serde_json::json!({ "delta": "hello" }),
            thread_id: Some("t1".to_string()),
            turn_id: Some("turn1".to_string()),
            item_id: None,
            delta: Some("hello".to_string()),
        };
        assert!(classify_interaction(&event).is_none());
    }
}
