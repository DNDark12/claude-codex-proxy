use std::collections::HashMap;

use serde_json::Value;

use crate::domain::codex::{
    CodexErrorEvent, CodexUsage, ExtractedCodexEvent, FunctionCallDelta, FunctionCallDone,
    FunctionCallStart, ParsedSseEvent,
};

#[derive(Default)]
pub struct CodexEventExtractor {
    item_id_to_call: HashMap<String, (String, String)>,
}

impl CodexEventExtractor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn extract(&mut self, raw: ParsedSseEvent) -> ExtractedCodexEvent {
        match raw {
            ParsedSseEvent::Done => {
                let mut evt = ExtractedCodexEvent::empty("done".to_string());
                evt.is_done = true;
                evt
            }
            ParsedSseEvent::Json { event, payload } => self.extract_json(event, payload),
        }
    }

    fn extract_json(
        &mut self,
        fallback_event: Option<String>,
        payload: Value,
    ) -> ExtractedCodexEvent {
        let event_type = payload
            .get("type")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or(fallback_event)
            .unwrap_or_else(|| "unknown".to_string());

        let mut out = ExtractedCodexEvent::empty(event_type.clone());
        out.response_id = extract_response_id(&payload);

        if let Some(u) = extract_usage(&payload) {
            out.usage = Some(u);
        }

        match event_type.as_str() {
            "response.output_text.delta" => {
                out.text_delta = payload
                    .get("delta")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                    .or_else(|| payload.as_str().map(ToString::to_string));
            }
            "response.output_text.done" => {
                out.text_delta = payload
                    .get("text")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                    .or_else(|| payload.as_str().map(ToString::to_string));
            }
            "response.output_item.delta" => {
                out.text_delta = payload
                    .get("delta")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                    .or_else(|| {
                        payload
                            .get("item")
                            .and_then(|v| v.get("output_text"))
                            .and_then(|v| v.get("delta"))
                            .and_then(Value::as_str)
                            .map(ToString::to_string)
                    });
            }
            "response.reasoning_summary_text.delta" => {
                out.reasoning_delta = payload
                    .get("delta")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                    .or_else(|| payload.as_str().map(ToString::to_string));
            }
            "response.reasoning_summary_text.done" => {
                out.reasoning_delta = payload
                    .get("text")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                    .or_else(|| payload.as_str().map(ToString::to_string));
            }
            "response.output_item.added" => {
                if let Some(item) = payload.get("item") {
                    if item.get("type").and_then(Value::as_str) == Some("function_call") {
                        let call_id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .or_else(|| item.get("id").and_then(Value::as_str))
                            .unwrap_or("unknown_call")
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string();
                        let output_index = payload
                            .get("output_index")
                            .and_then(Value::as_u64)
                            .unwrap_or_default()
                            as usize;

                        if let Some(item_id) = item.get("id").and_then(Value::as_str) {
                            self.item_id_to_call
                                .insert(item_id.to_string(), (call_id.clone(), name.clone()));
                        }

                        out.function_call_start = Some(FunctionCallStart {
                            call_id,
                            name,
                            output_index,
                        });
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                let ref_id = payload
                    .get("call_id")
                    .and_then(Value::as_str)
                    .or_else(|| payload.get("item_id").and_then(Value::as_str))
                    .unwrap_or("unknown");

                let call_id = self
                    .item_id_to_call
                    .get(ref_id)
                    .map(|(call_id, _)| call_id.clone())
                    .unwrap_or_else(|| ref_id.to_string());

                if let Some(delta) = payload.get("delta").and_then(Value::as_str) {
                    out.function_call_delta = Some(FunctionCallDelta {
                        call_id,
                        delta: delta.to_string(),
                    });
                }
            }
            "response.function_call_arguments.done" => {
                let ref_id = payload
                    .get("call_id")
                    .and_then(Value::as_str)
                    .or_else(|| payload.get("item_id").and_then(Value::as_str))
                    .unwrap_or("unknown");

                let (resolved_call_id, resolved_name) = self
                    .item_id_to_call
                    .get(ref_id)
                    .cloned()
                    .unwrap_or_else(|| (ref_id.to_string(), String::new()));

                let name = payload
                    .get("name")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| {
                        if resolved_name.is_empty() {
                            "unknown".to_string()
                        } else {
                            resolved_name
                        }
                    });

                out.function_call_done = Some(FunctionCallDone {
                    call_id: resolved_call_id,
                    name,
                    arguments: payload
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}")
                        .to_string(),
                });
            }
            "response.output_item.done" => {
                if let Some(item) = payload.get("item") {
                    if item.get("type").and_then(Value::as_str) == Some("function_call") {
                        let call_id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .or_else(|| item.get("id").and_then(Value::as_str))
                            .unwrap_or("unknown_call")
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string();
                        let arguments = item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}")
                            .to_string();
                        out.function_call_done = Some(FunctionCallDone {
                            call_id,
                            name,
                            arguments,
                        });
                    }
                }
            }
            "error" | "response.failed" => {
                out.error = Some(if let Some(message) = payload.as_str() {
                    CodexErrorEvent {
                        code: "upstream_error".to_string(),
                        message: message.to_string(),
                    }
                } else {
                    let err_obj = payload.get("error").unwrap_or(&payload);
                    CodexErrorEvent {
                        code: err_obj
                            .get("code")
                            .and_then(Value::as_str)
                            .unwrap_or("upstream_error")
                            .to_string(),
                        message: err_obj
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown upstream error")
                            .to_string(),
                    }
                });
                out.is_done = true;
            }
            "response.completed" => {
                out.is_done = true;
            }
            "response.incomplete" => {
                out.is_done = true;
            }
            _ => {}
        }

        out
    }
}

fn extract_response_id(payload: &Value) -> Option<String> {
    payload
        .get("response")
        .and_then(|r| r.get("id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            payload
                .get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

fn extract_usage(payload: &Value) -> Option<CodexUsage> {
    let usage_obj = payload
        .get("response")
        .and_then(|r| r.get("usage"))
        .or_else(|| payload.get("usage"))?;

    let input_tokens = usage_obj
        .get("input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0) as i32;
    let output_tokens = usage_obj
        .get("output_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0) as i32;

    let cached_tokens = usage_obj
        .get("cached_tokens")
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .or_else(|| {
            usage_obj
                .get("input_tokens_details")
                .and_then(|v| v.get("cached_tokens"))
                .and_then(Value::as_i64)
                .map(|v| v as i32)
        });

    let reasoning_tokens = usage_obj
        .get("reasoning_tokens")
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .or_else(|| {
            usage_obj
                .get("output_tokens_details")
                .and_then(|v| v.get("reasoning_tokens"))
                .and_then(Value::as_i64)
                .map(|v| v as i32)
        });

    Some(CodexUsage {
        input_tokens,
        output_tokens,
        cached_tokens,
        reasoning_tokens,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn maps_function_call_item_id_to_call_id() {
        let mut extractor = CodexEventExtractor::new();

        let start = extractor.extract(ParsedSseEvent::Json {
            event: None,
            payload: json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "id": "item_1",
                    "type": "function_call",
                    "call_id": "call_abc",
                    "name": "read_file"
                }
            }),
        });
        assert!(start.function_call_start.is_some());

        let delta = extractor.extract(ParsedSseEvent::Json {
            event: None,
            payload: json!({
                "type": "response.function_call_arguments.delta",
                "call_id": "item_1",
                "delta": "{\"path\":"
            }),
        });

        assert_eq!(
            delta
                .function_call_delta
                .as_ref()
                .map(|v| v.call_id.as_str()),
            Some("call_abc")
        );
    }

    #[test]
    fn extracts_text_from_output_item_delta() {
        let mut extractor = CodexEventExtractor::new();
        let event = extractor.extract(ParsedSseEvent::Json {
            event: None,
            payload: json!({
                "type": "response.output_item.delta",
                "item": {
                    "output_text": { "delta": "hello" }
                }
            }),
        });

        assert_eq!(event.text_delta.as_deref(), Some("hello"));
    }

    #[test]
    fn extracts_text_from_output_text_done() {
        let mut extractor = CodexEventExtractor::new();
        let event = extractor.extract(ParsedSseEvent::Json {
            event: Some("response.output_text.done".to_string()),
            payload: json!({
                "text": "complete"
            }),
        });

        assert_eq!(event.text_delta.as_deref(), Some("complete"));
    }

    #[test]
    fn extracts_usage_from_nested_details() {
        let mut extractor = CodexEventExtractor::new();
        let event = extractor.extract(ParsedSseEvent::Json {
            event: Some("response.completed".to_string()),
            payload: json!({
                "response": {
                    "id": "resp_1",
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 20,
                        "input_tokens_details": {
                            "cached_tokens": 3
                        },
                        "output_tokens_details": {
                            "reasoning_tokens": 7
                        }
                    }
                }
            }),
        });

        let usage = event.usage.expect("usage");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.cached_tokens, Some(3));
        assert_eq!(usage.reasoning_tokens, Some(7));
    }
}
