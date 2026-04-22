use crate::domain::codex::{
    CodexContentPart, CodexInputItem, CodexMessageContent, CodexResponsesRequest, CodexTextFormat,
    CodexTextFormatType,
};
use crate::domain::openai::{
    ChatCompletionsRequest, OpenAIContent, OpenAIContentPart, OpenAIImageUrl,
};
use crate::model_profiles::resolve_model_profile;
use crate::translation::tool_format::{
    openai_functions_to_codex, openai_tool_choice_to_codex, openai_tools_to_codex,
};

pub fn translate_openai_to_codex(req: &ChatCompletionsRequest) -> CodexResponsesRequest {
    let resolved_model = resolve_model_profile(&req.model);
    let instructions = build_instructions(req);
    let mut input: Vec<CodexInputItem> = Vec::new();

    for message in &req.messages {
        match message.role.as_str() {
            "system" | "developer" => continue,
            "assistant" => {
                let text = extract_text(message.content.as_ref());
                if !text.is_empty()
                    || (message.tool_calls.is_none() && message.function_call.is_none())
                {
                    input.push(CodexInputItem::Message {
                        role: "assistant".to_string(),
                        content: CodexMessageContent::Text(text),
                    });
                }

                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        input.push(CodexInputItem::FunctionCall {
                            item_type: "function_call".to_string(),
                            call_id: call.id.clone(),
                            name: call.function.name.clone(),
                            arguments: call.function.arguments.clone(),
                        });
                    }
                }

                if let Some(call) = &message.function_call {
                    input.push(CodexInputItem::FunctionCall {
                        item_type: "function_call".to_string(),
                        call_id: format!("fc_{}", call.name),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    });
                }
            }
            "tool" => {
                input.push(CodexInputItem::FunctionCallOutput {
                    item_type: "function_call_output".to_string(),
                    call_id: message
                        .tool_call_id
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                    output: extract_text(message.content.as_ref()),
                });
            }
            "function" => {
                input.push(CodexInputItem::FunctionCallOutput {
                    item_type: "function_call_output".to_string(),
                    call_id: format!(
                        "fc_{}",
                        message
                            .name
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string())
                    ),
                    output: extract_text(message.content.as_ref()),
                });
            }
            _ => {
                input.push(CodexInputItem::Message {
                    role: "user".to_string(),
                    content: extract_message_content(message.content.as_ref()),
                });
            }
        }
    }

    if input.is_empty() {
        input.push(CodexInputItem::Message {
            role: "user".to_string(),
            content: CodexMessageContent::Text(String::new()),
        });
    }

    let tools = if let Some(tools) = &req.tools {
        let mapped = openai_tools_to_codex(tools);
        (!mapped.is_empty()).then_some(mapped)
    } else if let Some(functions) = &req.functions {
        let mapped = openai_functions_to_codex(functions);
        (!mapped.is_empty()).then_some(mapped)
    } else {
        None
    };

    let tool_choice = req
        .tool_choice
        .as_ref()
        .and_then(openai_tool_choice_to_codex);

    let text = map_response_format(req);
    let reasoning_effort = effective_openai_reasoning_effort(req);

    CodexResponsesRequest {
        model: resolved_model.backend_model,
        instructions,
        input,
        tools,
        tool_choice,
        reasoning: reasoning_effort.as_ref().map(|effort| {
            serde_json::json!({
                "summary": "auto",
                "effort": effort
            })
        }),
        store: false,
        stream: true,
        text,
    }
}

pub fn effective_openai_reasoning_effort(req: &ChatCompletionsRequest) -> Option<String> {
    req.reasoning_effort
        .clone()
        .or_else(|| resolve_model_profile(&req.model).effort)
}

fn build_instructions(req: &ChatCompletionsRequest) -> String {
    let instructions = req
        .messages
        .iter()
        .filter(|m| m.role == "system" || m.role == "developer")
        .map(|m| extract_text(m.content.as_ref()))
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    if instructions.is_empty() {
        "You are a helpful assistant.".to_string()
    } else {
        instructions
    }
}

fn extract_text(content: Option<&OpenAIContent>) -> String {
    let Some(content) = content else {
        return String::new();
    };

    match content {
        OpenAIContent::Text(text) => text.clone(),
        OpenAIContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                OpenAIContentPart::Text { text } => text.clone(),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn extract_message_content(content: Option<&OpenAIContent>) -> CodexMessageContent {
    let Some(content) = content else {
        return CodexMessageContent::Text(String::new());
    };

    match content {
        OpenAIContent::Text(text) => CodexMessageContent::Text(text.clone()),
        OpenAIContent::Parts(parts) => {
            let mut mapped_parts: Vec<CodexContentPart> = Vec::new();
            for part in parts {
                match part {
                    OpenAIContentPart::Text { text } => {
                        if let Some(text) = text {
                            mapped_parts.push(CodexContentPart::InputText { text: text.clone() });
                        }
                    }
                    OpenAIContentPart::ImageUrl { image_url } => {
                        let url = match image_url {
                            OpenAIImageUrl::Url(url) => url.clone(),
                            OpenAIImageUrl::Object { url } => url.clone(),
                        };
                        mapped_parts.push(CodexContentPart::InputImage { image_url: url });
                    }
                }
            }

            if mapped_parts.len() == 1 {
                if let Some(CodexContentPart::InputText { text }) = mapped_parts.first().cloned() {
                    return CodexMessageContent::Text(text);
                }
            }

            CodexMessageContent::Parts(mapped_parts)
        }
    }
}

fn map_response_format(req: &ChatCompletionsRequest) -> Option<CodexTextFormat> {
    let response_format = req.response_format.as_ref()?;

    match response_format.format_type.as_str() {
        "text" => None,
        "json_object" => Some(CodexTextFormat {
            format: CodexTextFormatType {
                format_type: "json_object".to_string(),
                name: None,
                schema: None,
                strict: None,
            },
        }),
        "json_schema" => {
            let schema = response_format.json_schema.as_ref()?;
            Some(CodexTextFormat {
                format: CodexTextFormatType {
                    format_type: "json_schema".to_string(),
                    name: Some(schema.name.clone()),
                    schema: Some(schema.schema.clone()),
                    strict: schema.strict,
                },
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::domain::openai::{OpenAIMessage, OpenAIResponseFormat};

    #[test]
    fn maps_high_reasoning_model_alias_to_base_model_and_effort() {
        let req = ChatCompletionsRequest {
            model: "gpt-5.2-codex-high".to_string(),
            messages: vec![OpenAIMessage {
                role: "user".to_string(),
                content: Some(OpenAIContent::Text("hi".to_string())),
                tool_calls: None,
                function_call: None,
                tool_call_id: None,
                name: None,
            }],
            stream: Some(false),
            tools: None,
            tool_choice: None,
            functions: None,
            reasoning_effort: None,
            response_format: None,
        };

        let out = translate_openai_to_codex(&req);

        assert_eq!(out.model, "gpt-5.2-codex");
        assert_eq!(
            out.reasoning,
            Some(json!({
                "summary": "auto",
                "effort": "high"
            }))
        );
    }

    #[test]
    fn preserves_explicit_openai_reasoning_effort_over_model_alias() {
        let req = ChatCompletionsRequest {
            model: "gpt-5.2-codex-xhigh".to_string(),
            messages: vec![OpenAIMessage {
                role: "user".to_string(),
                content: Some(OpenAIContent::Text("hi".to_string())),
                tool_calls: None,
                function_call: None,
                tool_call_id: None,
                name: None,
            }],
            stream: Some(false),
            tools: None,
            tool_choice: None,
            functions: None,
            reasoning_effort: Some("low".to_string()),
            response_format: Some(OpenAIResponseFormat {
                format_type: "text".to_string(),
                json_schema: None,
            }),
        };

        let out = translate_openai_to_codex(&req);

        assert_eq!(out.model, "gpt-5.2-codex");
        assert_eq!(
            out.reasoning,
            Some(json!({
                "summary": "auto",
                "effort": "low"
            }))
        );
    }
}
