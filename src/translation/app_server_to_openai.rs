use chrono::Utc;
use std::convert::Infallible;
use warp::sse::Event;

use crate::app_server::{AppServerEvent, AppServerEventKind};
use crate::domain::openai::{
    ChatCompletionsResponse, OpenAIChoice, OpenAIChunk, OpenAIChunkChoice, OpenAIChunkDelta,
    OpenAIChunkToolCall, OpenAIChunkToolFunction, OpenAIFunctionCall, OpenAIResponseMessage,
    OpenAIResponseToolCall, OpenAIUsage,
};
use crate::translation::tool_runtime::{ToolCallAssembler, ToolRegistry};

fn error_text(message: &str) -> String {
    format!("[Error] {message}")
}

pub fn collect_app_server_to_openai(
    completion_id: &str,
    model: &str,
    events: &[AppServerEvent],
    tool_registry: Option<ToolRegistry>,
) -> ChatCompletionsResponse {
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
                        .unwrap_or_else(|| serde_json::json!({}))
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

    let tool_calls = assembler
        .finalize_all()
        .into_iter()
        .filter(|decision| decision.emit)
        .map(|decision| OpenAIResponseToolCall {
            id: decision.call_id,
            call_type: "function".to_string(),
            function: OpenAIFunctionCall {
                name: decision.display_tool_name,
                arguments: decision.input_json,
            },
        })
        .collect::<Vec<_>>();

    if let Some(error) = terminal_error {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&error_text(&error));
    }
    if text.is_empty() && tool_calls.is_empty() {
        text = error_text("Codex returned an empty response");
    }

    ChatCompletionsResponse {
        id: completion_id.to_string(),
        object: "chat.completion".to_string(),
        created: Utc::now().timestamp(),
        model: model.to_string(),
        choices: vec![OpenAIChoice {
            index: 0,
            message: OpenAIResponseMessage {
                role: "assistant".to_string(),
                content: (!text.is_empty()).then_some(text),
                tool_calls: (!tool_calls.is_empty()).then_some(tool_calls.clone()),
            },
            finish_reason: if tool_calls.is_empty() {
                "stop".to_string()
            } else {
                "tool_calls".to_string()
            },
        }],
        usage: OpenAIUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        },
    }
}

pub fn render_openai_sse_events(
    completion_id: &str,
    model: &str,
    events: Vec<AppServerEvent>,
) -> Vec<Event> {
    let created = Utc::now().timestamp();
    let mut out = vec![json_event(&OpenAIChunk {
        id: completion_id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        choices: vec![OpenAIChunkChoice {
            index: 0,
            delta: OpenAIChunkDelta {
                role: Some("assistant".to_string()),
                content: None,
                tool_calls: None,
            },
            finish_reason: None,
        }],
        usage: None,
    })];

    let mut next_tool_index = 0usize;
    let mut has_valid_tool_calls = false;

    for event in events {
        match event.kind {
            AppServerEventKind::AgentMessageDelta => {
                out.push(json_event(&OpenAIChunk {
                    id: completion_id.to_string(),
                    object: "chat.completion.chunk".to_string(),
                    created,
                    model: model.to_string(),
                    choices: vec![OpenAIChunkChoice {
                        index: 0,
                        delta: OpenAIChunkDelta {
                            role: None,
                            content: Some(event.delta.unwrap_or_default()),
                            tool_calls: None,
                        },
                        finish_reason: None,
                    }],
                    usage: None,
                }));
            }
            AppServerEventKind::ItemStarted if event.item_type() == Some("function_call") => {
                out.push(json_event(&OpenAIChunk {
                    id: completion_id.to_string(),
                    object: "chat.completion.chunk".to_string(),
                    created,
                    model: model.to_string(),
                    choices: vec![OpenAIChunkChoice {
                        index: 0,
                        delta: OpenAIChunkDelta {
                            role: None,
                            content: None,
                            tool_calls: Some(vec![OpenAIChunkToolCall {
                                index: next_tool_index,
                                id: event.item_id.clone(),
                                call_type: Some("function".to_string()),
                                function: OpenAIChunkToolFunction {
                                    name: event.tool_name().map(str::to_string),
                                    arguments: Some(String::new()),
                                },
                            }]),
                        },
                        finish_reason: None,
                    }],
                    usage: None,
                }));
            }
            AppServerEventKind::ItemCompleted if event.item_type() == Some("function_call") => {
                out.push(json_event(&OpenAIChunk {
                    id: completion_id.to_string(),
                    object: "chat.completion.chunk".to_string(),
                    created,
                    model: model.to_string(),
                    choices: vec![OpenAIChunkChoice {
                        index: 0,
                        delta: OpenAIChunkDelta {
                            role: None,
                            content: None,
                            tool_calls: Some(vec![OpenAIChunkToolCall {
                                index: next_tool_index,
                                id: None,
                                call_type: None,
                                function: OpenAIChunkToolFunction {
                                    name: None,
                                    arguments: Some(
                                        event
                                            .tool_arguments_json()
                                            .map(|value| value.to_string())
                                            .unwrap_or_default(),
                                    ),
                                },
                            }]),
                        },
                        finish_reason: None,
                    }],
                    usage: None,
                }));
                next_tool_index += 1;
                has_valid_tool_calls = true;
            }
            AppServerEventKind::TurnCompleted => {
                out.push(json_event(&OpenAIChunk {
                    id: completion_id.to_string(),
                    object: "chat.completion.chunk".to_string(),
                    created,
                    model: model.to_string(),
                    choices: vec![OpenAIChunkChoice {
                        index: 0,
                        delta: OpenAIChunkDelta {
                            role: None,
                            content: None,
                            tool_calls: None,
                        },
                        finish_reason: Some(if has_valid_tool_calls {
                            "tool_calls".to_string()
                        } else {
                            "stop".to_string()
                        }),
                    }],
                    usage: None,
                }));
                out.push(Event::default().data("[DONE]"));
            }
            AppServerEventKind::Error => {
                out.push(json_event(&OpenAIChunk {
                    id: completion_id.to_string(),
                    object: "chat.completion.chunk".to_string(),
                    created,
                    model: model.to_string(),
                    choices: vec![OpenAIChunkChoice {
                        index: 0,
                        delta: OpenAIChunkDelta {
                            role: None,
                            content: Some(error_text(
                                &event
                                    .error_message()
                                    .unwrap_or_else(|| "app-server error".to_string()),
                            )),
                            tool_calls: None,
                        },
                        finish_reason: Some("stop".to_string()),
                    }],
                    usage: None,
                }));
                out.push(Event::default().data("[DONE]"));
            }
            _ => {}
        }
    }

    out
}

pub fn stream_executor_job_to_openai(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<AppServerEvent>,
    completion_id: String,
    model: String,
) -> impl futures_core::Stream<Item = Result<Event, Infallible>> + Send {
    async_stream::stream! {
        let created = Utc::now().timestamp();
        yield Ok(json_event(&OpenAIChunk {
            id: completion_id.clone(),
            object: "chat.completion.chunk".to_string(),
            created,
            model: model.clone(),
            choices: vec![OpenAIChunkChoice {
                index: 0,
                delta: OpenAIChunkDelta {
                    role: Some("assistant".to_string()),
                    content: None,
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
        }));

        let mut next_tool_index = 0usize;
        let mut has_valid_tool_calls = false;

        while let Some(event) = rx.recv().await {
            match event.kind {
                AppServerEventKind::AgentMessageDelta => {
                    yield Ok(json_event(&OpenAIChunk {
                        id: completion_id.clone(),
                        object: "chat.completion.chunk".to_string(),
                        created,
                        model: model.clone(),
                        choices: vec![OpenAIChunkChoice {
                            index: 0,
                            delta: OpenAIChunkDelta {
                                role: None,
                                content: Some(event.delta.unwrap_or_default()),
                                tool_calls: None,
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                    }));
                }
                AppServerEventKind::ItemStarted if event.item_type() == Some("function_call") => {
                    yield Ok(json_event(&OpenAIChunk {
                        id: completion_id.clone(),
                        object: "chat.completion.chunk".to_string(),
                        created,
                        model: model.clone(),
                        choices: vec![OpenAIChunkChoice {
                            index: 0,
                            delta: OpenAIChunkDelta {
                                role: None,
                                content: None,
                                tool_calls: Some(vec![OpenAIChunkToolCall {
                                    index: next_tool_index,
                                    id: event.item_id.clone(),
                                    call_type: Some("function".to_string()),
                                    function: OpenAIChunkToolFunction {
                                        name: event.tool_name().map(str::to_string),
                                        arguments: Some(String::new()),
                                    },
                                }]),
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                    }));
                }
                AppServerEventKind::ItemCompleted if event.item_type() == Some("function_call") => {
                    yield Ok(json_event(&OpenAIChunk {
                        id: completion_id.clone(),
                        object: "chat.completion.chunk".to_string(),
                        created,
                        model: model.clone(),
                        choices: vec![OpenAIChunkChoice {
                            index: 0,
                            delta: OpenAIChunkDelta {
                                role: None,
                                content: None,
                                tool_calls: Some(vec![OpenAIChunkToolCall {
                                    index: next_tool_index,
                                    id: None,
                                    call_type: None,
                                    function: OpenAIChunkToolFunction {
                                        name: None,
                                        arguments: Some(
                                            event.tool_arguments_json().map(|value| value.to_string()).unwrap_or_default(),
                                        ),
                                    },
                                }]),
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                    }));
                    next_tool_index += 1;
                    has_valid_tool_calls = true;
                }
                AppServerEventKind::TurnCompleted => {
                    yield Ok(json_event(&OpenAIChunk {
                        id: completion_id.clone(),
                        object: "chat.completion.chunk".to_string(),
                        created,
                        model: model.clone(),
                        choices: vec![OpenAIChunkChoice {
                            index: 0,
                            delta: OpenAIChunkDelta {
                                role: None,
                                content: None,
                                tool_calls: None,
                            },
                            finish_reason: Some(if has_valid_tool_calls {
                                "tool_calls".to_string()
                            } else {
                                "stop".to_string()
                            }),
                        }],
                        usage: None,
                    }));
                    yield Ok(Event::default().data("[DONE]"));
                    break;
                }
                AppServerEventKind::Error => {
                    yield Ok(json_event(&OpenAIChunk {
                        id: completion_id.clone(),
                        object: "chat.completion.chunk".to_string(),
                        created,
                        model: model.clone(),
                        choices: vec![OpenAIChunkChoice {
                            index: 0,
                            delta: OpenAIChunkDelta {
                                role: None,
                                content: Some(error_text(
                                    &event
                                        .error_message()
                                        .unwrap_or_else(|| "app-server error".to_string()),
                                )),
                                tool_calls: None,
                            },
                            finish_reason: Some("stop".to_string()),
                        }],
                        usage: None,
                    }));
                    yield Ok(Event::default().data("[DONE]"));
                    break;
                }
                _ => {}
            }
        }
    }
}

fn json_event<T: serde::Serialize>(payload: &T) -> Event {
    Event::default().data(serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::{AppServerEvent, AppServerEventKind};
    use serde_json::json;

    #[test]
    fn agent_message_delta_emits_openai_chunk() {
        let event = AppServerEvent {
            method: "item/agentMessage/delta".to_string(),
            kind: AppServerEventKind::AgentMessageDelta,
            params: json!({ "threadId": "t1", "turnId": "u1", "delta": "hello" }),
            thread_id: Some("t1".to_string()),
            turn_id: Some("u1".to_string()),
            item_id: Some("i1".to_string()),
            delta: Some("hello".to_string()),
        };

        let rendered = render_openai_sse_events("chatcmpl-1", "gpt-5.4", vec![event]);
        let debug = rendered
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(debug.contains("chat.completion.chunk"));
        assert!(debug.contains("hello"));
    }

    #[test]
    fn item_started_function_call_emits_tool_call_chunk() {
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

        let rendered = render_openai_sse_events("chatcmpl-1", "gpt-5.4", vec![event]);
        let debug = rendered
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(debug.contains("tool_calls"));
        assert!(debug.contains("Read"));
    }

    #[test]
    fn error_event_emits_openai_error_chunk() {
        let event = AppServerEvent {
            method: "error".to_string(),
            kind: AppServerEventKind::Error,
            params: json!({ "threadId": "t1", "turnId": "u1", "message": "quota exceeded" }),
            thread_id: Some("t1".to_string()),
            turn_id: Some("u1".to_string()),
            item_id: None,
            delta: None,
        };

        let rendered = render_openai_sse_events("chatcmpl-1", "gpt-5.4", vec![event]);
        let debug = rendered
            .iter()
            .map(|evt| format!("{evt:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(debug.contains("[Error] quota exceeded"));
        assert!(debug.contains("[DONE]"));
    }
}
