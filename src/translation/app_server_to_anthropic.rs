use serde_json::json;
use std::convert::Infallible;
use warp::sse::Event;

use crate::app_server::{AppServerEvent, AppServerEventKind};
use crate::domain::anthropic::{
    AnthropicMessagesResponse, AnthropicResponseContentBlock, AnthropicUsage,
};
use crate::translation::tool_runtime::{ToolCallAssembler, ToolRegistry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenBlock {
    None,
    Text,
    Tool,
}

pub fn collect_app_server_to_anthropic(
    message_id: &str,
    model: &str,
    events: &[AppServerEvent],
    tool_registry: Option<ToolRegistry>,
) -> AnthropicMessagesResponse {
    let mut text = String::new();
    let mut terminal_error = None;
    let mut assembler = ToolCallAssembler::new(tool_registry);

    for event in events {
        match event.kind {
            AppServerEventKind::AgentMessageDelta => {
                if let Some(delta) = event.delta.as_deref() {
                    text.push_str(delta);
                }
            }
            AppServerEventKind::ItemStarted if event.item_type() == Some("function_call") => {
                if let (Some(call_id), Some(tool_name)) = (event.item_id.clone(), event.tool_name())
                {
                    assembler.on_start(call_id, tool_name.to_string());
                }
            }
            AppServerEventKind::ItemCompleted if event.item_type() == Some("function_call") => {
                if let Some(call_id) = event.item_id.clone() {
                    let arguments = event
                        .tool_arguments_json()
                        .cloned()
                        .unwrap_or_else(|| json!({}))
                        .to_string();
                    assembler.on_done(
                        call_id,
                        event.tool_name().unwrap_or_default().to_string(),
                        arguments,
                    );
                }
            }
            AppServerEventKind::Error => {
                terminal_error = event.error_message();
            }
            _ => {}
        }
    }

    let decisions = assembler.finalize_all();
    let mut content = Vec::new();
    if let Some(error) = terminal_error {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&error_text(&error));
    }
    if text.is_empty() && decisions.is_empty() {
        text = error_text("Codex returned an empty response");
    }
    if !text.is_empty() {
        content.push(AnthropicResponseContentBlock::Text { text });
    }

    let mut emitted_tool_uses = false;
    for decision in decisions.into_iter().filter(|decision| decision.emit) {
        emitted_tool_uses = true;
        content.push(AnthropicResponseContentBlock::ToolUse {
            id: decision.call_id,
            name: decision.display_tool_name,
            input: decision.input_value.unwrap_or_else(|| json!({})),
        });
    }

    AnthropicMessagesResponse {
        id: message_id.to_string(),
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        model: model.to_string(),
        content,
        stop_reason: if emitted_tool_uses {
            "tool_use".to_string()
        } else {
            "end_turn".to_string()
        },
        stop_sequence: None,
        usage: AnthropicUsage {
            input_tokens: 0,
            output_tokens: 0,
        },
    }
}

pub fn render_anthropic_sse_events(
    message_id: &str,
    model: &str,
    events: Vec<AppServerEvent>,
) -> Vec<Event> {
    let mut out = vec![sse_event(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": message_id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": { "input_tokens": 0, "output_tokens": 0 }
            }
        }),
    )];

    let mut open_block = OpenBlock::None;
    let mut content_index = 0usize;
    let mut has_valid_tool_calls = false;

    for event in events {
        match event.kind {
            AppServerEventKind::AgentMessageDelta => {
                for evt in ensure_text_delta(
                    &mut open_block,
                    &mut content_index,
                    event.delta.as_deref().unwrap_or_default(),
                ) {
                    out.push(evt);
                }
            }
            AppServerEventKind::ItemStarted if event.item_type() == Some("function_call") => {
                if open_block == OpenBlock::Text {
                    out.push(close_block(content_index));
                    content_index += 1;
                }
                open_block = OpenBlock::Tool;
                out.push(sse_event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": content_index,
                        "content_block": {
                            "type": "tool_use",
                            "id": event.item_id.clone().unwrap_or_default(),
                            "name": event.tool_name().unwrap_or_default(),
                            "input": {}
                        }
                    }),
                ));
            }
            AppServerEventKind::ItemCompleted if event.item_type() == Some("function_call") => {
                if let Some(arguments) = event.tool_arguments_json() {
                    out.push(sse_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": content_index,
                            "delta": {
                                "type": "input_json_delta",
                                "partial_json": arguments.to_string()
                            }
                        }),
                    ));
                }
                out.push(close_block(content_index));
                content_index += 1;
                open_block = OpenBlock::None;
                has_valid_tool_calls = true;
            }
            AppServerEventKind::TurnCompleted => {
                if matches!(open_block, OpenBlock::Text | OpenBlock::Tool) {
                    out.push(close_block(content_index));
                    content_index += 1;
                    open_block = OpenBlock::None;
                }
                finish_message_events(has_valid_tool_calls, &mut out);
            }
            AppServerEventKind::Error => {
                if matches!(open_block, OpenBlock::Text | OpenBlock::Tool) {
                    out.push(close_block(content_index));
                    content_index += 1;
                    open_block = OpenBlock::None;
                }
                for evt in ensure_text_delta(
                    &mut open_block,
                    &mut content_index,
                    &error_text(
                        &event
                            .error_message()
                            .unwrap_or_else(|| "app-server error".to_string()),
                    ),
                ) {
                    out.push(evt);
                }
                out.push(close_block(content_index));
                content_index += 1;
                open_block = OpenBlock::None;
                finish_message_events(false, &mut out);
            }
            _ => {}
        }
    }

    out
}

pub fn stream_executor_job_to_anthropic(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<AppServerEvent>,
    message_id: String,
    model: String,
) -> impl futures_core::Stream<Item = Result<Event, Infallible>> + Send {
    async_stream::stream! {
        yield Ok(sse_event(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 }
                }
            }),
        ));

        let mut open_block = OpenBlock::None;
        let mut content_index = 0usize;
        let mut has_valid_tool_calls = false;

        while let Some(event) = rx.recv().await {
            match event.kind {
                AppServerEventKind::AgentMessageDelta => {
                    for rendered in ensure_text_delta(
                        &mut open_block,
                        &mut content_index,
                        event.delta.as_deref().unwrap_or_default(),
                    ) {
                        yield Ok(rendered);
                    }
                }
                AppServerEventKind::ItemStarted if event.item_type() == Some("function_call") => {
                    if open_block == OpenBlock::Text {
                        yield Ok(close_block(content_index));
                        content_index += 1;
                    }
                    open_block = OpenBlock::Tool;
                    yield Ok(sse_event(
                        "content_block_start",
                        json!({
                            "type": "content_block_start",
                            "index": content_index,
                            "content_block": {
                                "type": "tool_use",
                                "id": event.item_id.clone().unwrap_or_default(),
                                "name": event.tool_name().unwrap_or_default(),
                                "input": {}
                            }
                        }),
                    ));
                }
                AppServerEventKind::ItemCompleted if event.item_type() == Some("function_call") => {
                    if let Some(arguments) = event.tool_arguments_json() {
                        yield Ok(sse_event(
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": content_index,
                                "delta": {
                                    "type": "input_json_delta",
                                    "partial_json": arguments.to_string()
                                }
                            }),
                        ));
                    }
                    yield Ok(close_block(content_index));
                    content_index += 1;
                    open_block = OpenBlock::None;
                    has_valid_tool_calls = true;
                }
                AppServerEventKind::TurnCompleted => {
                    if matches!(open_block, OpenBlock::Text | OpenBlock::Tool) {
                        yield Ok(close_block(content_index));
                    }
                    let mut tail = Vec::new();
                    finish_message_events(has_valid_tool_calls, &mut tail);
                    for event in tail {
                        yield Ok(event);
                    }
                    break;
                }
                AppServerEventKind::Error => {
                    if matches!(open_block, OpenBlock::Text | OpenBlock::Tool) {
                        yield Ok(close_block(content_index));
                        content_index += 1;
                    }
                    for rendered in ensure_text_delta(
                        &mut open_block,
                        &mut content_index,
                        &error_text(&event.error_message().unwrap_or_else(|| "app-server error".to_string())),
                    ) {
                        yield Ok(rendered);
                    }
                    yield Ok(close_block(content_index));
                    let mut tail = Vec::new();
                    finish_message_events(false, &mut tail);
                    for event in tail {
                        yield Ok(event);
                    }
                    break;
                }
                _ => {}
            }
        }
    }
}

fn sse_event(event_type: &str, data: serde_json::Value) -> Event {
    Event::default()
        .event(event_type)
        .data(serde_json::to_string(&data).unwrap_or_else(|_| "{}".to_string()))
}

fn close_block(index: usize) -> Event {
    sse_event(
        "content_block_stop",
        json!({
            "type": "content_block_stop",
            "index": index
        }),
    )
}

fn ensure_text_delta(
    open_block: &mut OpenBlock,
    content_index: &mut usize,
    text: &str,
) -> Vec<Event> {
    let mut events = Vec::new();

    if *open_block != OpenBlock::Text {
        if *open_block == OpenBlock::Tool {
            events.push(close_block(*content_index));
            *content_index += 1;
        }
        events.push(sse_event(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": *content_index,
                "content_block": {
                    "type": "text",
                    "text": ""
                }
            }),
        ));
        *open_block = OpenBlock::Text;
    }

    events.push(sse_event(
        "content_block_delta",
        json!({
            "type": "content_block_delta",
            "index": *content_index,
            "delta": {
                "type": "text_delta",
                "text": text
            }
        }),
    ));

    events
}

fn error_text(message: &str) -> String {
    format!("[Error] {message}")
}

fn finish_message_events(has_valid_tool_calls: bool, out: &mut Vec<Event>) {
    out.push(sse_event(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": if has_valid_tool_calls { "tool_use" } else { "end_turn" },
                "stop_sequence": null
            },
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0
            }
        }),
    ));
    out.push(sse_event("message_stop", json!({ "type": "message_stop" })));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::{AppServerEvent, AppServerEventKind};
    use serde_json::json;

    #[test]
    fn agent_message_delta_emits_content_block_delta() {
        let event = AppServerEvent {
            method: "item/agentMessage/delta".to_string(),
            kind: AppServerEventKind::AgentMessageDelta,
            params: json!({ "threadId": "t1", "turnId": "u1", "delta": "hello" }),
            thread_id: Some("t1".to_string()),
            turn_id: Some("u1".to_string()),
            item_id: Some("i1".to_string()),
            delta: Some("hello".to_string()),
        };

        let rendered = render_anthropic_sse_events("msg_1", "gpt-5.4", vec![event]);
        let debug = rendered
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(debug.contains("content_block_delta"));
        assert!(debug.contains("hello"));
    }

    #[test]
    fn item_started_function_call_emits_tool_use_start() {
        let event = AppServerEvent {
            method: "item/started".to_string(),
            kind: AppServerEventKind::ItemStarted,
            params: json!({
                "threadId": "t1",
                "turnId": "u1",
                "item": { "id": "tool-1", "type": "function_call", "toolName": "Read" }
            }),
            thread_id: Some("t1".to_string()),
            turn_id: Some("u1".to_string()),
            item_id: Some("tool-1".to_string()),
            delta: None,
        };

        let rendered = render_anthropic_sse_events("msg_1", "gpt-5.4", vec![event]);
        let debug = rendered
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(debug.contains("tool_use"));
        assert!(debug.contains("Read"));
    }

    #[test]
    fn turn_completed_emits_message_stop() {
        let event = AppServerEvent {
            method: "turn/completed".to_string(),
            kind: AppServerEventKind::TurnCompleted,
            params: json!({ "threadId": "t1", "turnId": "u1" }),
            thread_id: Some("t1".to_string()),
            turn_id: Some("u1".to_string()),
            item_id: None,
            delta: None,
        };

        let rendered = render_anthropic_sse_events("msg_1", "gpt-5.4", vec![event]);
        let debug = rendered
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(debug.contains("message_stop"));
    }

    #[test]
    fn error_event_emits_visible_error_text() {
        let event = AppServerEvent {
            method: "error".to_string(),
            kind: AppServerEventKind::Error,
            params: json!({ "threadId": "t1", "turnId": "u1", "message": "quota exceeded" }),
            thread_id: Some("t1".to_string()),
            turn_id: Some("u1".to_string()),
            item_id: None,
            delta: None,
        };

        let rendered = render_anthropic_sse_events("msg_1", "gpt-5.4", vec![event]);
        let debug = rendered
            .iter()
            .map(|evt| format!("{evt:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(debug.contains("[Error] quota exceeded"));
        assert!(debug.contains("message_stop"));
    }
}
