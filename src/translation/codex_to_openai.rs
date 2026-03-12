use std::collections::{HashMap, HashSet};
use std::convert::Infallible;

use anyhow::Result;
use chrono::Utc;
use futures_util::StreamExt;
use reqwest::Response;
use uuid::Uuid;
use warp::sse::Event;

use crate::domain::codex::{CodexUsage, FunctionCallDone};
use crate::domain::openai::{
    ChatCompletionsResponse, OpenAIChoice, OpenAIChunk, OpenAIChunkChoice, OpenAIChunkDelta,
    OpenAIChunkToolCall, OpenAIChunkToolFunction, OpenAIResponseMessage, OpenAIResponseToolCall,
    OpenAIUsage,
};
use crate::proxy::event_extractor::CodexEventExtractor;
use crate::proxy::sse_parser::parse_sse_response;

pub fn stream_codex_to_openai(
    response: Response,
    model: String,
    trace_id: String,
) -> impl futures_core::Stream<Item = std::result::Result<Event, Infallible>> + Send {
    async_stream::stream! {
        let mut extractor = CodexEventExtractor::new();
        let mut sse_stream = Box::pin(parse_sse_response(response));

        let chunk_id = format!("chatcmpl-{}", Uuid::new_v4());
        let created = Utc::now().timestamp();

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
        let mut tool_names_by_call_id: HashMap<String, String> = HashMap::new();
        let mut started_tool_calls: HashSet<String> = HashSet::new();
        let mut call_ids_with_deltas: HashSet<String> = HashSet::new();
        let mut next_tool_index = 0usize;
        let mut has_tool_calls = false;
        let mut has_content = false;
        let mut usage: Option<CodexUsage> = None;
        let mut final_sent = false;

        while let Some(item) = sse_stream.next().await {
            let Ok(raw) = item else {
                let err_chunk = make_text_chunk(&chunk_id, &model, "[Error] Failed to parse upstream stream.", created);
                yield Ok(Event::default().data(err_chunk));
                break;
            };

            let extracted = extractor.extract(raw);

            if let Some(err) = extracted.error {
                log::warn!("[{trace_id}] upstream error: {} - {}", err.code, err.message);
                let err_chunk = make_text_chunk(&chunk_id, &model, &format!("[Error] {}: {}", err.code, err.message), created);
                yield Ok(Event::default().data(err_chunk));
                break;
            }

            if let Some(start) = extracted.function_call_start {
                has_tool_calls = true;
                has_content = true;
                let call_id = start.call_id;
                let name = start.name;
                tool_names_by_call_id.insert(call_id.clone(), name.clone());
                let idx = *tool_call_indices.entry(call_id.clone()).or_insert_with(|| {
                    let current = next_tool_index;
                    next_tool_index += 1;
                    current
                });
                if started_tool_calls.insert(call_id.clone()) {
                    let chunk =
                        make_tool_start_chunk(&chunk_id, &model, created, idx, &call_id, &name);
                    yield Ok(Event::default().data(
                        serde_json::to_string(&chunk).unwrap_or_default(),
                    ));
                }
                continue;
            }

            if let Some(delta) = extracted.function_call_delta {
                has_tool_calls = true;
                has_content = true;
                let call_id = delta.call_id;
                let idx = *tool_call_indices.entry(call_id.clone()).or_insert_with(|| {
                    let current = next_tool_index;
                    next_tool_index += 1;
                    current
                });
                if !started_tool_calls.contains(&call_id) {
                    let name = "unknown".to_string();
                    tool_names_by_call_id.insert(call_id.clone(), name.clone());
                    let start_chunk =
                        make_tool_start_chunk(&chunk_id, &model, created, idx, &call_id, &name);
                    yield Ok(Event::default().data(
                        serde_json::to_string(&start_chunk).unwrap_or_default(),
                    ));
                    started_tool_calls.insert(call_id.clone());
                }
                call_ids_with_deltas.insert(call_id.clone());

                let chunk = OpenAIChunk {
                    id: chunk_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created,
                    model: model.clone(),
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
                                    arguments: Some(delta.delta),
                                },
                            }]),
                        },
                        finish_reason: None,
                    }],
                    usage: None,
                };
                yield Ok(Event::default().data(serde_json::to_string(&chunk).unwrap_or_default()));
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
                tool_names_by_call_id
                    .entry(call_id.clone())
                    .or_insert_with(|| name.clone());
                let idx = *tool_call_indices.entry(call_id.clone()).or_insert_with(|| {
                    let current = next_tool_index;
                    next_tool_index += 1;
                    current
                });
                if !started_tool_calls.contains(&call_id) {
                    let start_chunk =
                        make_tool_start_chunk(&chunk_id, &model, created, idx, &call_id, &name);
                    yield Ok(Event::default().data(
                        serde_json::to_string(&start_chunk).unwrap_or_default(),
                    ));
                    started_tool_calls.insert(call_id.clone());
                }
                if call_ids_with_deltas.contains(&call_id) {
                    continue;
                }

                let chunk = OpenAIChunk {
                    id: chunk_id.clone(),
                    object: "chat.completion.chunk".to_string(),
                    created,
                    model: model.clone(),
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
                                    arguments: Some(done.arguments),
                                },
                            }]),
                        },
                        finish_reason: None,
                    }],
                    usage: None,
                };
                yield Ok(Event::default().data(serde_json::to_string(&chunk).unwrap_or_default()));
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
                let final_chunk = make_final_chunk(&chunk_id, &model, created, has_tool_calls, usage.clone());
                yield Ok(Event::default().data(serde_json::to_string(&final_chunk).unwrap_or_default()));
                final_sent = true;
                break;
            }
        }

        if !has_content {
            let err_chunk = make_text_chunk(&chunk_id, &model, "[Error] Codex returned an empty response.", created);
            yield Ok(Event::default().data(err_chunk));
        }

        if !final_sent {
            let final_chunk = make_final_chunk(&chunk_id, &model, created, has_tool_calls, usage.clone());
            yield Ok(Event::default().data(serde_json::to_string(&final_chunk).unwrap_or_default()));
        }

        yield Ok(Event::default().data("[DONE]"));
    }
}

pub async fn collect_codex_to_openai(
    response: Response,
    model: String,
) -> Result<ChatCompletionsResponse> {
    let mut extractor = CodexEventExtractor::new();
    let mut sse_stream = Box::pin(parse_sse_response(response));

    let mut text = String::new();
    let mut tool_calls: Vec<FunctionCallDone> = Vec::new();
    let mut tool_call_seen: HashSet<String> = HashSet::new();
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
            if tool_call_seen.insert(done.call_id.clone()) {
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

    let finish_reason = if tool_calls.is_empty() {
        "stop".to_string()
    } else {
        "tool_calls".to_string()
    };

    let message = OpenAIResponseMessage {
        role: "assistant".to_string(),
        content: if text.is_empty() { None } else { Some(text) },
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(
                tool_calls
                    .into_iter()
                    .map(|call| OpenAIResponseToolCall {
                        id: call.call_id,
                        call_type: "function".to_string(),
                        function: crate::domain::openai::OpenAIFunctionCall {
                            name: call.name,
                            arguments: call.arguments,
                        },
                    })
                    .collect(),
            )
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
