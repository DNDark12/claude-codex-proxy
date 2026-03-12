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
use crate::domain::codex::CodexUsage;
use crate::proxy::event_extractor::CodexEventExtractor;
use crate::proxy::sse_parser::parse_sse_response;
use crate::translation::tool_runtime::{
    log_tool_decision, ToolCallAssembler, ToolCallDecision, ToolRegistry,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenBlock {
    None,
    Text,
}

pub fn stream_codex_to_anthropic(
    response: Response,
    model: String,
    trace_id: String,
    tool_registry: Option<ToolRegistry>,
) -> impl futures_core::Stream<Item = std::result::Result<Event, Infallible>> + Send {
    async_stream::stream! {
        let mut extractor = CodexEventExtractor::new();
        let mut assembler = ToolCallAssembler::new(tool_registry);
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
        let mut has_valid_tool_calls = false;
        let mut has_content = false;
        let mut response_id: Option<String> = None;

        while let Some(item) = sse_stream.next().await {
            let Ok(raw) = item else {
                for evt in ensure_text_delta(&mut open_block, &mut content_index, "[Error] Failed to parse upstream stream.") {
                    yield Ok(evt);
                }
                has_content = true;
                break;
            };

            let extracted = extractor.extract(raw);
            if let Some(id) = extracted.response_id.clone() {
                response_id = Some(id);
            }

            if let Some(err) = extracted.error {
                log::warn!("[{trace_id}] upstream error: {} - {}", err.code, err.message);
                for evt in ensure_text_delta(
                    &mut open_block,
                    &mut content_index,
                    &format!("[Error] {}: {}", err.code, err.message),
                ) {
                    yield Ok(evt);
                }
                has_content = true;
                break;
            }

            if let Some(start) = extracted.function_call_start {
                assembler.on_start(start.call_id, start.name);
                continue;
            }

            if let Some(delta) = extracted.function_call_delta {
                assembler.on_delta(delta.call_id, delta.delta);
                continue;
            }

            if let Some(done) = extracted.function_call_done {
                let call_id = done.call_id.clone();
                assembler.on_done(call_id.clone(), done.name, done.arguments);
                if let Some(decision) = assembler.finalize_call(&call_id) {
                    log_tool_decision(
                        &trace_id,
                        &trace_id,
                        response_id.as_deref(),
                        "anthropic_stream_done",
                        &decision,
                    );

                    for evt in materialize_tool_decision(
                        &decision,
                        &trace_id,
                        &mut open_block,
                        &mut content_index,
                        &mut has_valid_tool_calls,
                        &mut has_content,
                    ) {
                        yield Ok(evt);
                    }
                }
                continue;
            }

            if let Some(delta) = extracted.text_delta {
                has_content = true;
                for evt in ensure_text_delta(&mut open_block, &mut content_index, &delta) {
                    yield Ok(evt);
                }
            }

            if let Some(u) = extracted.usage {
                usage = u;
            }

            if extracted.is_done {
                break;
            }
        }

        for decision in assembler.finalize_all() {
            log_tool_decision(
                &trace_id,
                &trace_id,
                response_id.as_deref(),
                "anthropic_stream_force_finalize",
                &decision,
            );

            for evt in materialize_tool_decision(
                &decision,
                &trace_id,
                &mut open_block,
                &mut content_index,
                &mut has_valid_tool_calls,
                &mut has_content,
            ) {
                yield Ok(evt);
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
                "stop_reason": if has_valid_tool_calls { "tool_use" } else { "end_turn" },
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
    trace_id: &str,
    tool_registry: Option<ToolRegistry>,
) -> Result<AnthropicMessagesResponse> {
    let mut extractor = CodexEventExtractor::new();
    let mut assembler = ToolCallAssembler::new(tool_registry);
    let mut sse_stream = Box::pin(parse_sse_response(response));

    let mut text = String::new();
    let mut usage = CodexUsage {
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: None,
        reasoning_tokens: None,
    };
    let mut response_id: Option<String> = None;

    while let Some(item) = sse_stream.next().await {
        let raw = item?;
        let extracted = extractor.extract(raw);

        if let Some(id) = extracted.response_id.clone() {
            response_id = Some(id);
        }

        if let Some(err) = extracted.error {
            anyhow::bail!("{}: {}", err.code, err.message);
        }

        if let Some(start) = extracted.function_call_start {
            assembler.on_start(start.call_id, start.name);
        }

        if let Some(delta) = extracted.function_call_delta {
            assembler.on_delta(delta.call_id, delta.delta);
        }

        if let Some(done) = extracted.function_call_done {
            assembler.on_done(done.call_id, done.name, done.arguments);
        }

        if let Some(delta) = extracted.text_delta {
            text.push_str(&delta);
        }

        if let Some(u) = extracted.usage {
            usage = u;
        }
    }

    let mut emitted_calls: Vec<(String, String, serde_json::Value)> = Vec::new();

    for decision in assembler.finalize_all() {
        log_tool_decision(
            trace_id,
            trace_id,
            response_id.as_deref(),
            "anthropic_collect_finalize",
            &decision,
        );

        if decision.emit {
            emitted_calls.push((
                decision.call_id,
                decision.tool_name,
                decision.input_value.unwrap_or_else(|| json!({})),
            ));
            continue;
        }

        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&tool_skip_diagnostic_text(&decision, trace_id));
    }

    if text.is_empty() && emitted_calls.is_empty() {
        anyhow::bail!("Codex returned an empty response");
    }

    let mut content: Vec<AnthropicResponseContentBlock> = Vec::new();
    if !text.is_empty() {
        content.push(AnthropicResponseContentBlock::Text { text });
    }

    for (call_id, name, input) in emitted_calls {
        content.push(AnthropicResponseContentBlock::ToolUse {
            id: call_id,
            name,
            input,
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

fn materialize_tool_decision(
    decision: &ToolCallDecision,
    trace_id: &str,
    open_block: &mut OpenBlock,
    content_index: &mut usize,
    has_valid_tool_calls: &mut bool,
    has_content: &mut bool,
) -> Vec<Event> {
    if decision.emit {
        let mut events = Vec::new();
        if *open_block != OpenBlock::None {
            events.push(close_block(*content_index));
            *content_index += 1;
            *open_block = OpenBlock::None;
        }

        events.push(start_tool_block(
            *content_index,
            &decision.call_id,
            &decision.tool_name,
        ));
        events.push(event(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": *content_index,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": decision.input_json
                }
            }),
        ));
        events.push(close_block(*content_index));
        *content_index += 1;
        *has_valid_tool_calls = true;
        *has_content = true;
        return events;
    }

    *has_content = true;
    ensure_text_delta(
        open_block,
        content_index,
        &tool_skip_diagnostic_text(decision, trace_id),
    )
}

fn tool_skip_diagnostic_text(decision: &ToolCallDecision, trace_id: &str) -> String {
    let short_trace = &trace_id[..trace_id.len().min(8)];
    format!(
        "[Tool skipped: invalid parameters for {} (trace={short_trace})]",
        decision.tool_name
    )
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
