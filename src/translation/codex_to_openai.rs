use std::collections::HashMap;
use std::convert::Infallible;

use anyhow::Result;
use chrono::Utc;
use futures_util::StreamExt;
use reqwest::Response;
use uuid::Uuid;
use warp::sse::Event;

use crate::domain::codex::CodexUsage;
use crate::domain::openai::{
    ChatCompletionsResponse, OpenAIChoice, OpenAIChunk, OpenAIChunkChoice, OpenAIChunkDelta,
    OpenAIChunkToolCall, OpenAIChunkToolFunction, OpenAIResponseMessage, OpenAIResponseToolCall,
    OpenAIUsage,
};
use crate::proxy::event_extractor::CodexEventExtractor;
use crate::proxy::sse_parser::parse_sse_response;
use crate::translation::tool_runtime::{
    log_tool_decision, ToolCallAssembler, ToolCallDecision, ToolRegistry,
};

struct OpenAIToolChunkContext {
    chunk_id: String,
    model: String,
    created: i64,
    trace_id: String,
}

struct OpenAIToolChunkState<'a> {
    tool_call_indices: &'a mut HashMap<String, usize>,
    next_tool_index: &'a mut usize,
    has_valid_tool_calls: &'a mut bool,
    has_content: &'a mut bool,
}

pub fn stream_codex_to_openai(
    response: Response,
    model: String,
    trace_id: String,
    tool_registry: Option<ToolRegistry>,
) -> impl futures_core::Stream<Item = std::result::Result<Event, Infallible>> + Send {
    async_stream::stream! {
        let mut extractor = CodexEventExtractor::new();
        let mut assembler = ToolCallAssembler::new(tool_registry);
        let mut sse_stream = Box::pin(parse_sse_response(response));

        let chunk_id = format!("chatcmpl-{}", Uuid::new_v4());
        let created = Utc::now().timestamp();
        let tool_chunk_context = OpenAIToolChunkContext {
            chunk_id: chunk_id.clone(),
            model: model.clone(),
            created,
            trace_id: trace_id.clone(),
        };

        let role_chunk = OpenAIChunk {
            id: chunk_id.clone(),
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
        };

        yield Ok(Event::default().data(serde_json::to_string(&role_chunk).unwrap_or_default()));

        let mut tool_call_indices: HashMap<String, usize> = HashMap::new();
        let mut next_tool_index = 0usize;
        let mut has_valid_tool_calls = false;
        let mut has_content = false;
        let mut usage: Option<CodexUsage> = None;
        let mut final_sent = false;
        let mut response_id: Option<String> = None;

        while let Some(item) = sse_stream.next().await {
            let Ok(raw) = item else {
                let err_chunk = make_text_chunk(&chunk_id, &model, "[Error] Failed to parse upstream stream.", created);
                yield Ok(Event::default().data(err_chunk));
                has_content = true;
                break;
            };

            let extracted = extractor.extract(raw);
            if let Some(id) = extracted.response_id.clone() {
                response_id = Some(id);
            }

            if let Some(err) = extracted.error {
                log::warn!("[{trace_id}] upstream error: {} - {}", err.code, err.message);
                let err_chunk = make_text_chunk(&chunk_id, &model, &format!("[Error] {}: {}", err.code, err.message), created);
                yield Ok(Event::default().data(err_chunk));
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
                        "openai_stream_done",
                        &decision,
                    );

                    for chunk in materialize_openai_tool_decision(
                        &decision,
                        &tool_chunk_context,
                        OpenAIToolChunkState {
                            tool_call_indices: &mut tool_call_indices,
                            next_tool_index: &mut next_tool_index,
                            has_valid_tool_calls: &mut has_valid_tool_calls,
                            has_content: &mut has_content,
                        },
                    ) {
                        yield Ok(Event::default().data(chunk));
                    }
                }
                continue;
            }

            if let Some(text_delta) = extracted.text_delta {
                has_content = true;
                let chunk = OpenAIChunk {
                    id: chunk_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created,
                    model: model.clone(),
                    choices: vec![OpenAIChunkChoice {
                        index: 0,
                        delta: OpenAIChunkDelta {
                            role: None,
                            content: Some(text_delta),
                            tool_calls: None,
                        },
                        finish_reason: None,
                    }],
                    usage: None,
                };
                yield Ok(Event::default().data(serde_json::to_string(&chunk).unwrap_or_default()));
            }

            if let Some(u) = extracted.usage {
                usage = Some(u);
            }

            if extracted.is_done {
                let final_chunk =
                    make_final_chunk(&chunk_id, &model, created, has_valid_tool_calls, usage.clone());
                yield Ok(Event::default().data(serde_json::to_string(&final_chunk).unwrap_or_default()));
                final_sent = true;
                break;
            }
        }

        for decision in assembler.finalize_all() {
            log_tool_decision(
                &trace_id,
                &trace_id,
                response_id.as_deref(),
                "openai_stream_force_finalize",
                &decision,
            );

            for chunk in materialize_openai_tool_decision(
                &decision,
                &tool_chunk_context,
                OpenAIToolChunkState {
                    tool_call_indices: &mut tool_call_indices,
                    next_tool_index: &mut next_tool_index,
                    has_valid_tool_calls: &mut has_valid_tool_calls,
                    has_content: &mut has_content,
                },
            ) {
                yield Ok(Event::default().data(chunk));
            }
        }

        if !has_content {
            let err_chunk = make_text_chunk(&chunk_id, &model, "[Error] Codex returned an empty response.", created);
            yield Ok(Event::default().data(err_chunk));
        }

        if !final_sent {
            let final_chunk = make_final_chunk(&chunk_id, &model, created, has_valid_tool_calls, usage.clone());
            yield Ok(Event::default().data(serde_json::to_string(&final_chunk).unwrap_or_default()));
        }

        yield Ok(Event::default().data("[DONE]"));
    }
}

pub async fn collect_codex_to_openai(
    response: Response,
    model: String,
    trace_id: &str,
    tool_registry: Option<ToolRegistry>,
) -> Result<ChatCompletionsResponse> {
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

    let mut emitted_tool_calls: Vec<OpenAIResponseToolCall> = Vec::new();

    for decision in assembler.finalize_all() {
        log_tool_decision(
            trace_id,
            trace_id,
            response_id.as_deref(),
            "openai_collect_finalize",
            &decision,
        );

        if decision.emit {
            emitted_tool_calls.push(OpenAIResponseToolCall {
                id: decision.call_id,
                call_type: "function".to_string(),
                function: crate::domain::openai::OpenAIFunctionCall {
                    name: decision.display_tool_name,
                    arguments: decision.input_json,
                },
            });
            continue;
        }

        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&tool_skip_diagnostic_text(&decision, trace_id));
    }

    if text.is_empty() && emitted_tool_calls.is_empty() {
        anyhow::bail!("Codex returned an empty response");
    }

    let finish_reason = if emitted_tool_calls.is_empty() {
        "stop".to_string()
    } else {
        "tool_calls".to_string()
    };

    let message = OpenAIResponseMessage {
        role: "assistant".to_string(),
        content: if text.is_empty() { None } else { Some(text) },
        tool_calls: if emitted_tool_calls.is_empty() {
            None
        } else {
            Some(emitted_tool_calls)
        },
    };

    Ok(ChatCompletionsResponse {
        id: format!("chatcmpl-{}", Uuid::new_v4()),
        object: "chat.completion".to_string(),
        created: Utc::now().timestamp(),
        model,
        choices: vec![OpenAIChoice {
            index: 0,
            message,
            finish_reason,
        }],
        usage: OpenAIUsage {
            prompt_tokens: usage.input_tokens,
            completion_tokens: usage.output_tokens,
            total_tokens: usage.input_tokens + usage.output_tokens,
        },
    })
}

fn materialize_openai_tool_decision(
    decision: &ToolCallDecision,
    context: &OpenAIToolChunkContext,
    state: OpenAIToolChunkState<'_>,
) -> Vec<String> {
    if decision.emit {
        let idx = *state
            .tool_call_indices
            .entry(decision.call_id.clone())
            .or_insert_with(|| {
                let current = *state.next_tool_index;
                *state.next_tool_index += 1;
                current
            });

        let start_chunk = make_tool_start_chunk(
            &context.chunk_id,
            &context.model,
            context.created,
            idx,
            &decision.call_id,
            &decision.display_tool_name,
        );

        let args_chunk = OpenAIChunk {
            id: context.chunk_id.clone(),
            object: "chat.completion.chunk".to_string(),
            created: context.created,
            model: context.model.clone(),
            choices: vec![OpenAIChunkChoice {
                index: 0,
                delta: OpenAIChunkDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![OpenAIChunkToolCall {
                        index: idx,
                        id: None,
                        call_type: None,
                        function: OpenAIChunkToolFunction {
                            name: None,
                            arguments: Some(decision.input_json.clone()),
                        },
                    }]),
                },
                finish_reason: None,
            }],
            usage: None,
        };

        *state.has_valid_tool_calls = true;
        *state.has_content = true;

        return vec![
            serde_json::to_string(&start_chunk).unwrap_or_default(),
            serde_json::to_string(&args_chunk).unwrap_or_default(),
        ];
    }

    *state.has_content = true;
    vec![make_text_chunk(
        &context.chunk_id,
        &context.model,
        &tool_skip_diagnostic_text(decision, &context.trace_id),
        context.created,
    )]
}

fn tool_skip_diagnostic_text(decision: &ToolCallDecision, trace_id: &str) -> String {
    let short_trace = &trace_id[..trace_id.len().min(8)];
    format!(
        "[Tool skipped: invalid parameters for {} (trace={short_trace})]",
        decision.display_tool_name
    )
}

fn make_text_chunk(chunk_id: &str, model: &str, text: &str, created: i64) -> String {
    let chunk = OpenAIChunk {
        id: chunk_id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        choices: vec![OpenAIChunkChoice {
            index: 0,
            delta: OpenAIChunkDelta {
                role: None,
                content: Some(text.to_string()),
                tool_calls: None,
            },
            finish_reason: None,
        }],
        usage: None,
    };

    serde_json::to_string(&chunk).unwrap_or_else(|_| "{}".to_string())
}

fn make_tool_start_chunk(
    chunk_id: &str,
    model: &str,
    created: i64,
    index: usize,
    call_id: &str,
    name: &str,
) -> OpenAIChunk {
    OpenAIChunk {
        id: chunk_id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        choices: vec![OpenAIChunkChoice {
            index: 0,
            delta: OpenAIChunkDelta {
                role: None,
                content: None,
                tool_calls: Some(vec![OpenAIChunkToolCall {
                    index,
                    id: Some(call_id.to_string()),
                    call_type: Some("function".to_string()),
                    function: OpenAIChunkToolFunction {
                        name: Some(name.to_string()),
                        arguments: Some(String::new()),
                    },
                }]),
            },
            finish_reason: None,
        }],
        usage: None,
    }
}

fn make_final_chunk(
    chunk_id: &str,
    model: &str,
    created: i64,
    has_tool_calls: bool,
    usage: Option<CodexUsage>,
) -> OpenAIChunk {
    OpenAIChunk {
        id: chunk_id.to_string(),
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
            finish_reason: Some(if has_tool_calls {
                "tool_calls".to_string()
            } else {
                "stop".to_string()
            }),
        }],
        usage: usage.map(|u| OpenAIUsage {
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            total_tokens: u.input_tokens + u.output_tokens,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_start_chunk_contains_id_name_and_function_type() {
        let chunk = make_tool_start_chunk("chatcmpl-1", "gpt-5.4", 123, 0, "call_1", "read_file");
        let rendered = serde_json::to_string(&chunk).expect("json");
        assert!(rendered.contains("\"id\":\"call_1\""));
        assert!(rendered.contains("\"name\":\"read_file\""));
        assert!(rendered.contains("\"type\":\"function\""));
    }
}
