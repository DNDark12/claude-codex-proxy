use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::jsonrpc::JsonRpcNotification;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppServerEventKind {
    ThreadStarted,
    TurnStarted,
    TurnCompleted,
    ItemStarted,
    ItemCompleted,
    AgentMessageDelta,
    PlanDelta,
    Error,
    TerminalInteraction,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppServerEvent {
    pub method: String,
    pub kind: AppServerEventKind,
    pub params: Value,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub item_id: Option<String>,
    pub delta: Option<String>,
}

impl From<JsonRpcNotification> for AppServerEvent {
    fn from(notification: JsonRpcNotification) -> Self {
        let kind = match notification.method.as_str() {
            "thread/started" => AppServerEventKind::ThreadStarted,
            "turn/started" => AppServerEventKind::TurnStarted,
            "turn/completed" => AppServerEventKind::TurnCompleted,
            "item/started" => AppServerEventKind::ItemStarted,
            "item/completed" => AppServerEventKind::ItemCompleted,
            "item/agentMessage/delta" => AppServerEventKind::AgentMessageDelta,
            "item/plan/delta" => AppServerEventKind::PlanDelta,
            "item/commandExecution/terminalInteraction" => AppServerEventKind::TerminalInteraction,
            "error" => AppServerEventKind::Error,
            "terminal_interaction" => AppServerEventKind::TerminalInteraction,
            _ => AppServerEventKind::Unknown,
        };

        let thread_id = notification
            .params
            .get("threadId")
            .or_else(|| notification.params.get("conversationId"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let turn_id = notification
            .params
            .get("turnId")
            .or_else(|| notification.params.get("turn").and_then(|turn| turn.get("id")))
            .and_then(Value::as_str)
            .map(str::to_string);
        let item_id = notification
            .params
            .get("itemId")
            .or_else(|| notification.params.get("item").and_then(|item| item.get("id")))
            .and_then(Value::as_str)
            .map(str::to_string);
        let delta = notification
            .params
            .get("delta")
            .and_then(Value::as_str)
            .map(str::to_string);

        Self {
            method: notification.method,
            kind,
            params: notification.params,
            thread_id,
            turn_id,
            item_id,
            delta,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::jsonrpc::JsonRpcNotification;

    #[test]
    fn extracts_nested_turn_and_item_identifiers() {
        let event = AppServerEvent::from(JsonRpcNotification {
            jsonrpc: "2.0".to_string(),
            method: "item/completed".to_string(),
            params: serde_json::json!({
                "threadId": "thread-1",
                "turn": { "id": "turn-1" },
                "item": { "id": "item-1" }
            }),
        });

        assert_eq!(event.thread_id.as_deref(), Some("thread-1"));
        assert_eq!(event.turn_id.as_deref(), Some("turn-1"));
        assert_eq!(event.item_id.as_deref(), Some("item-1"));
    }
}
