use std::collections::{HashMap, HashSet};
use std::convert::Infallible;

use anyhow::Result;
use futures_util::StreamExt;
use reqwest::Response;
use serde_json::json;
use uuid::Uuid;
use warp::sse::Event;

use crate::domain::anthropic::{
    AnthropicMessagesResponse, AnthropicResponseContentBlock, AnthropicUsage,
};
use crate::domain::codex::{CodexUsage, FunctionCallDone};
use crate::proxy::event_extractor::CodexEventExtractor;
use crate::proxy::sse_parser::parse_sse_response;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenBlock {
    None,
    Text,
    Tool,
}

pub fn stream_codex_to_anthropic(
    response: Response,
    model: String,
    trace_id: String,
) -> impl futures_core::Stream<Item = std::result::Result<Event, Infallible>> + Send {
    async_stream::stream! {
        let mut extractor = CodexEventExtractor::new();
        let mut sse_stream = Box::pin(parse_sse_response(response));
        let message_id = format!("msg_{}", Uuid::new_v4().simple());

        yield Ok(event("message_start", json!({
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
        })));

        let mut content_index = 0usize;
        let mut open_block = OpenBlock::None;
        let mut usage = CodexUsage {
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: None,
            reasoning_tokens: None,
        };
        let mut has_tool_calls = false;
        let mut has_content = false;
        let mut call_ids_with_deltas: HashSet<String> = HashSet::new();
        let mut tool_names_by_call_id: HashMap<String, String> = HashMap::new();
        let mut open_tool_call_id: Option<String> = None;

        while let Some(item) = sse_stream.next().await {
            let Ok(raw) = item else {
                for evt in ensure_text_delta(&mut open_block, &mut content_index, "[Error] Failed to parse upstream stream.") {
                    yield Ok(evt);
                }
                break;
            };

            let extracted = extractor.extract(raw);

            if let Some(err) = extracted.error {
                log::warn!("[{trace_id}] upstream error: {} - {}", err.code, err.message);
                for evt in ensure_text_delta(
                    &mut open_block,
                    &mut content_index,
                    &format!("[Error] {}: {}", err.code, err.message),
                ) {
                    yield Ok(evt);
                }
                break;
            }

            if let Some(start) = extracted.function_call_start {
                has_tool_calls = true;
                has_content = true;
                tool_names_by_call_id.insert(start.call_id.clone(), start.name.clone());

                if open_block != OpenBlock::None {
                    yield Ok(close_block(content_index));
                    content_index += 1;
                }

                yield Ok(start_tool_block(content_index, &start.call_id, &start.name));
                open_block = OpenBlock::Tool;
                open_tool_call_id = Some(start.call_id);
                continue;
            }

            if let Some(delta) = extracted.function_call_delta {
                has_tool_calls = true;
                has_content = true;
                let call_id = delta.call_id;

                if open_block != OpenBlock::Tool
                    || open_tool_call_id.as_deref() != Some(call_id.as_str())
                {
                    if open_block != OpenBlock::None {
                        yield Ok(close_block(content_index));
                        content_index += 1;
                    }

                    let fallback_name = tool_names_by_call_id
                        .get(&call_id)
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string());
                    yield Ok(start_tool_block(content_index, &call_id, &fallback_name));
                    open_block = OpenBlock::Tool;
                    open_tool_call_id = Some(call_id.clone());
                }

                call_ids_with_deltas.insert(call_id);

                yield Ok(event("content_block_delta", json!({
                    "type": "content_block_delta",
                    "index": content_index,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": delta.delta
                    }
                })));
                continue;
            }

            if let Some(done) = extracted.function_call_done {
                has_tool_calls = true;
                has_content = true;
                let call_id = done.call_id;
                let name = if done.name.is_empty() {
                    tool_names_by_call_id
                        .get(&call_id)
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string())
                } else {
                    done.name
                };
                tool_names_by_call_id.insert(call_id.clone(), name.clone());

                if open_block != OpenBlock::Tool
                    || open_tool_call_id.as_deref() != Some(call_id.as_str())
                {
                    if open_block != OpenBlock::None {
                        yield Ok(close_block(content_index));
                        content_index += 1;
                    }
                    yield Ok(start_tool_block(content_index, &call_id, &name));
                    open_block = OpenBlock::Tool;
                    open_tool_call_id = Some(call_id.clone());
                }

                if !call_ids_with_deltas.contains(&call_id) {
                    yield Ok(event("content_block_delta", json!({
                        "type": "content_block_delta",
                        "index": content_index,
                        "delta": {
                            "type": "input_json_delta",
                            "partial_json": done.arguments
                        }
                    })));
                }

                if open_block == OpenBlock::Tool
                    && open_tool_call_id.as_deref() == Some(call_id.as_str())
                {
                    yield Ok(close_block(content_index));
                    content_index += 1;
                    open_block = OpenBlock::None;
                    open_tool_call_id = None;
                }
                continue;
            }

            if let Some(delta) = extracted.text_delta {
                has_content = true;

                if open_block != OpenBlock::Text {
                    if open_block != OpenBlock::None {
                        yield Ok(close_block(content_index));
                        content_index += 1;
                        open_tool_call_id = None;
                    }

                    yield Ok(event("content_block_start", json!({
                        "type": "content_block_start",
                        "index": content_index,
                        "content_block": {
                            "type": "text",
                            "text": ""
                        }
                    })));
                    open_block = OpenBlock::Text;
                }

                yield Ok(event("content_block_delta", json!({
                    "type": "content_block_delta",
                    "index": content_index,
                    "delta": {
                        "type": "text_delta",
                        "text": delta
                    }
                })));
            }

            if let Some(u) = extracted.usage {
                usage = u;
            }

            if extracted.is_done {
                break;
            }
        }

        if !has_content {
            for evt in ensure_text_delta(
                &mut open_block,
                &mut content_index,
                "[Error] Codex returned an empty response.",
            ) {
                yield Ok(evt);
            }
        }

        if open_block != OpenBlock::None {
            yield Ok(close_block(content_index));
        }

        yield Ok(event("message_delta", json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": if has_tool_calls { "tool_use" } else { "end_turn" },
                "stop_sequence": null
            },
            "usage": {
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens
            }
        })));

        yield Ok(event("message_stop", json!({
            "type": "message_stop"
        })));
    }
}

pub async fn collect_codex_to_anthropic(
    response: Response,
    model: String,
) -> Result<AnthropicMessagesResponse> {
    let mut extractor = CodexEventExtractor::new();
    let mut sse_stream = Box::pin(parse_sse_response(response));

    let mut text = String::new();
    let mut tool_calls: Vec<FunctionCallDone> = Vec::new();
    let mut tool_seen: HashSet<String> = HashSet::new();
    let mut usage = CodexUsage {
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: None,
        reasoning_tokens: None,
    };

    while let Some(item) = sse_stream.next().await {
        let raw = item?;
        let extracted = extractor.extract(raw);

        if let Some(err) = extracted.error {
            anyhow::bail!("{}: {}", err.code, err.message);
        }

        if let Some(delta) = extracted.text_delta {
            text.push_str(&delta);
        }

        if let Some(done) = extracted.function_call_done {
            if tool_seen.insert(done.call_id.clone()) {
                tool_calls.push(done);
            }
        }

        if let Some(u) = extracted.usage {
            usage = u;
        }
    }

    if text.is_empty() && tool_calls.is_empty() {
        anyhow::bail!("Codex returned an empty response");
    }

    let mut content: Vec<AnthropicResponseContentBlock> = Vec::new();
    if !text.is_empty() {
        content.push(AnthropicResponseContentBlock::Text { text });
    }

    for call in tool_calls {
        let parsed_input: serde_json::Value =
            serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));
        content.push(AnthropicResponseContentBlock::ToolUse {
            id: call.call_id,
            name: call.name,
            input: parsed_input,
        });
    }

    Ok(AnthropicMessagesResponse {
        id: format!("msg_{}", Uuid::new_v4().simple()),
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        model,
        stop_reason: if content
            .iter()
            .any(|v| matches!(v, AnthropicResponseContentBlock::ToolUse { .. }))
        {
            "tool_use".to_string()
        } else {
            "end_turn".to_string()
        },
        stop_sequence: None,
        usage: AnthropicUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
        },
        content,
    })
}

fn event(event_type: &str, data: serde_json::Value) -> Event {
    Event::default()
        .event(event_type)
        .data(serde_json::to_string(&data).unwrap_or_else(|_| "{}".to_string()))
}

fn close_block(index: usize) -> Event {
    event(
        "content_block_stop",
        json!({
            "type": "content_block_stop",
            "index": index
        }),
    )
}

fn start_tool_block(index: usize, call_id: &str, name: &str) -> Event {
    event(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": index,
            "content_block": {
                "type": "tool_use",
                "id": call_id,
                "name": name,
                "input": {}
            }
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
        if *open_block != OpenBlock::None {
            events.push(close_block(*content_index));
            *content_index += 1;
        }
        events.push(event(
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

    events.push(event(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_tool_block_renders_valid_tool_use_event() {
        let rendered = start_tool_block(3, "call_1", "read_file").to_string();
        assert!(rendered.contains("event:content_block_start"));
        assert!(rendered.contains("\"type\":\"tool_use\""));
        assert!(rendered.contains("\"id\":\"call_1\""));
        assert!(rendered.contains("\"name\":\"read_file\""));
    }
}
