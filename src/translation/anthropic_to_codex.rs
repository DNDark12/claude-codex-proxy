use std::collections::HashMap;

use serde_json::{json, Value};

use crate::domain::anthropic::{
    AnthropicContent, AnthropicContentBlock, AnthropicMessagesRequest, AnthropicSystem,
};
use crate::domain::codex::{
    CodexContentPart, CodexInputItem, CodexMessageContent, CodexResponsesRequest,
};
use crate::model_profiles::resolve_model_profile;
use crate::skills::{ReferencePayload, ResolvedSkillContext, SkillMergeMode};
use crate::translation::tool_format::{anthropic_tool_choice_to_codex, anthropic_tools_to_codex};

pub fn translate_anthropic_to_codex(
    req: &AnthropicMessagesRequest,
    bridge: Option<&ResolvedSkillContext>,
) -> CodexResponsesRequest {
    let resolved_model = resolve_model_profile(&req.model);
    let mut input: Vec<CodexInputItem> = Vec::new();

    for message in &req.messages {
        match &message.content {
            AnthropicContent::Text(text) => {
                input.push(CodexInputItem::Message {
                    role: message.role.clone(),
                    content: CodexMessageContent::Text(text.clone()),
                });
            }
            AnthropicContent::Blocks(blocks) => {
                input.extend(convert_blocks_to_input_items(
                    &message.role,
                    blocks,
                    bridge.map(|bridge| &bridge.tool_aliases),
                ));
            }
        }
    }

    if input.is_empty() {
        input.push(CodexInputItem::Message {
            role: "user".to_string(),
            content: CodexMessageContent::Text(String::new()),
        });
    }

    let tools = req
        .tools
        .as_ref()
        .map(|v| anthropic_tools_to_codex(v, bridge.map(|bridge| &bridge.tool_aliases)))
        .filter(|v| !v.is_empty());

    let tool_choice = req
        .tool_choice
        .as_ref()
        .and_then(|choice| anthropic_tool_choice_to_codex(choice, bridge.map(|v| &v.tool_aliases)));

    CodexResponsesRequest {
        model: resolved_model.backend_model,
        instructions: build_instructions(req, bridge),
        input,
        tools,
        tool_choice,
        reasoning: effective_anthropic_reasoning_effort(req).map(reasoning_payload),
        store: false,
        stream: true,
        text: None,
    }
}

fn convert_blocks_to_input_items(
    role: &str,
    blocks: &[AnthropicContentBlock],
    aliases: Option<&HashMap<String, String>>,
) -> Vec<CodexInputItem> {
    let mut out = Vec::new();

    let mut user_parts: Vec<CodexContentPart> = Vec::new();
    let mut assistant_text = String::new();

    let flush = |out: &mut Vec<CodexInputItem>,
                 role: &str,
                 user_parts: &mut Vec<CodexContentPart>,
                 assistant_text: &mut String| {
        if role == "assistant" {
            if !assistant_text.is_empty() {
                out.push(CodexInputItem::Message {
                    role: "assistant".to_string(),
                    content: CodexMessageContent::Text(std::mem::take(assistant_text)),
                });
            }
            return;
        }

        if user_parts.is_empty() {
            return;
        }

        if user_parts.len() == 1 {
            if let Some(CodexContentPart::InputText { text }) = user_parts.first().cloned() {
                out.push(CodexInputItem::Message {
                    role: role.to_string(),
                    content: CodexMessageContent::Text(text),
                });
                user_parts.clear();
                return;
            }
        }

        out.push(CodexInputItem::Message {
            role: role.to_string(),
            content: CodexMessageContent::Parts(std::mem::take(user_parts)),
        });
    };

    for block in blocks {
        match block {
            AnthropicContentBlock::Text { text } => {
                if role == "assistant" {
                    assistant_text.push_str(text);
                } else {
                    user_parts.push(CodexContentPart::InputText { text: text.clone() });
                }
            }
            AnthropicContentBlock::Image { source } => {
                if role == "assistant" {
                    continue;
                }
                if source.source_type == "base64" {
                    if let (Some(media_type), Some(data)) = (&source.media_type, &source.data) {
                        user_parts.push(CodexContentPart::InputImage {
                            image_url: format!("data:{media_type};base64,{data}"),
                        });
                    }
                }
            }
            AnthropicContentBlock::ToolUse { id, name, input } => {
                flush(&mut out, role, &mut user_parts, &mut assistant_text);
                out.push(CodexInputItem::FunctionCall {
                    item_type: "function_call".to_string(),
                    call_id: id.clone(),
                    name: apply_tool_alias(name, aliases),
                    arguments: serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string()),
                });
            }
            AnthropicContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                flush(&mut out, role, &mut user_parts, &mut assistant_text);
                let mut output = anthropic_result_to_text(content);
                if is_error.unwrap_or(false) {
                    output = format!("Error: {output}");
                }
                out.push(CodexInputItem::FunctionCallOutput {
                    item_type: "function_call_output".to_string(),
                    call_id: tool_use_id.clone(),
                    output,
                });
            }
            AnthropicContentBlock::Unknown => {}
        }
    }

    flush(&mut out, role, &mut user_parts, &mut assistant_text);
    out
}

fn apply_tool_alias(name: &str, aliases: Option<&HashMap<String, String>>) -> String {
    let Some(aliases) = aliases else {
        return name.to_string();
    };

    aliases
        .get(name)
        .cloned()
        .or_else(|| {
            aliases
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())
        })
        .unwrap_or_else(|| name.to_string())
}

fn anthropic_result_to_text(content: &Value) -> String {
    match content {
        Value::String(v) => v.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| {
                if let Some(text) = v.as_str() {
                    return Some(text.to_string());
                }
                if let Some(text) = v.get("text").and_then(Value::as_str) {
                    return Some(text.to_string());
                }
                None
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => serde_json::to_string(content).unwrap_or_else(|_| String::new()),
    }
}

fn build_instructions(
    req: &AnthropicMessagesRequest,
    bridge: Option<&ResolvedSkillContext>,
) -> String {
    let base = build_base_instructions(req, bridge.is_none());
    let Some(bridge) = bridge else {
        return base;
    };

    let mut bridge_instruction = bridge.codex_instructions.trim().to_string();
    let reference_block = render_reference_block(&bridge.references);
    if !reference_block.is_empty() {
        if !bridge_instruction.is_empty() {
            bridge_instruction.push_str("\n\n");
        }
        bridge_instruction.push_str(&reference_block);
    }

    merge_instructions(&bridge_instruction, base, bridge.merge_mode)
}

fn build_base_instructions(req: &AnthropicMessagesRequest, include_default: bool) -> String {
    if let Some(system) = &req.system {
        match system {
            AnthropicSystem::Text(text) => return text.clone(),
            AnthropicSystem::Blocks(blocks) => {
                let v = blocks
                    .iter()
                    .filter_map(|b| b.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if !v.is_empty() {
                    return v;
                }
            }
        }
    }

    let message_systems = req
        .messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| match &m.content {
            AnthropicContent::Text(v) => v.clone(),
            AnthropicContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    AnthropicContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    if message_systems.is_empty() {
        if include_default {
            "You are a helpful assistant.".to_string()
        } else {
            String::new()
        }
    } else {
        message_systems
    }
}

fn merge_instructions(prefix: &str, base: String, merge_mode: SkillMergeMode) -> String {
    let prefix = prefix.trim();
    let base = base.trim().to_string();

    if prefix.is_empty() {
        return base;
    }

    match merge_mode {
        SkillMergeMode::Replace => prefix.to_string(),
        SkillMergeMode::Append => {
            if base.is_empty() {
                prefix.to_string()
            } else {
                format!("{base}\n\n{prefix}")
            }
        }
        SkillMergeMode::Prepend => {
            if base.is_empty() {
                prefix.to_string()
            } else {
                format!("{prefix}\n\n{base}")
            }
        }
    }
}

fn render_reference_block(references: &[ReferencePayload]) -> String {
    if references.is_empty() {
        return String::new();
    }

    let rendered = references
        .iter()
        .filter_map(|reference| {
            let content = reference.content.trim();
            if content.is_empty() {
                return None;
            }

            Some(format!(
                "## Skill Reference: {}\n\n{}",
                reference.path.trim(),
                content
            ))
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    rendered.trim().to_string()
}

pub fn effective_anthropic_reasoning_effort(req: &AnthropicMessagesRequest) -> Option<String> {
    req.thinking
        .as_ref()
        .and_then(map_thinking_to_effort)
        .or_else(|| resolve_model_profile(&req.model).effort)
}

fn reasoning_payload(effort: String) -> Value {
    json!({
        "summary": "auto",
        "effort": effort
    })
}

fn map_thinking_to_effort(thinking: &Value) -> Option<String> {
    let obj = thinking.as_object()?;
    let budget = obj
        .get("budget_tokens")
        .and_then(Value::as_i64)
        .or_else(|| obj.get("budgetTokens").and_then(Value::as_i64));

    Some(
        budget
            .map(|tokens| {
                if tokens >= 16000 {
                    "high"
                } else if tokens >= 4000 {
                    "medium"
                } else {
                    "low"
                }
            })
            .unwrap_or("medium")
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::domain::anthropic::{
        AnthropicMessage, AnthropicToolChoice, AnthropicToolChoiceObject,
    };
    use crate::domain::codex::CodexInputItem;

    #[test]
    fn supports_top_level_system() {
        let req = AnthropicMessagesRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("hi".to_string()),
            }],
            system: Some(AnthropicSystem::Text("RULE".to_string())),
            tools: None,
            tool_choice: None,
            stream: Some(false),
            thinking: None,
        };

        let out = translate_anthropic_to_codex(&req, None);
        assert_eq!(out.instructions, "RULE");
    }

    #[test]
    fn maps_tool_use_and_tool_result() {
        let req = AnthropicMessagesRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: AnthropicContent::Blocks(vec![AnthropicContentBlock::ToolUse {
                        id: "toolu_1".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path":"README.md"}),
                    }]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Blocks(vec![AnthropicContentBlock::ToolResult {
                        tool_use_id: "toolu_1".to_string(),
                        content: json!("ok"),
                        is_error: None,
                    }]),
                },
            ],
            system: None,
            tools: None,
            tool_choice: None,
            stream: Some(true),
            thinking: None,
        };

        let out = translate_anthropic_to_codex(&req, None);
        assert!(out
            .input
            .iter()
            .any(|item| matches!(item, CodexInputItem::FunctionCall { call_id, .. } if call_id == "toolu_1")));
        assert!(out
            .input
            .iter()
            .any(|item| matches!(item, CodexInputItem::FunctionCallOutput { call_id, .. } if call_id == "toolu_1")));
    }

    #[test]
    fn maps_tool_choice() {
        let req = AnthropicMessagesRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("x".to_string()),
            }],
            system: None,
            tools: None,
            tool_choice: Some(AnthropicToolChoice::Object(AnthropicToolChoiceObject {
                choice_type: "any".to_string(),
                name: None,
            })),
            stream: Some(true),
            thinking: None,
        };

        let out = translate_anthropic_to_codex(&req, None);
        assert!(
            matches!(out.tool_choice, Some(crate::domain::codex::CodexToolChoice::Strategy(v)) if v == "required")
        );
    }

    #[test]
    fn prepends_bridge_instructions_without_default_prompt() {
        let req = AnthropicMessagesRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("review".to_string()),
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: Some(false),
            thinking: None,
        };
        let bridge = ResolvedSkillContext {
            id: "code-review".to_string(),
            version: "1.0.0".to_string(),
            marker: "skill-bridge:code-review@1.0.0".to_string(),
            codex_instructions: "# Review\n\nLook for bugs.".to_string(),
            references: vec![ReferencePayload {
                path: "references/review-rubric.md".to_string(),
                content: "Check correctness first.".to_string(),
            }],
            merge_mode: SkillMergeMode::Prepend,
            tool_aliases: Default::default(),
        };

        let out = translate_anthropic_to_codex(&req, Some(&bridge));
        assert_eq!(
            out.instructions,
            "# Review\n\nLook for bugs.\n\n## Skill Reference: references/review-rubric.md\n\nCheck correctness first."
        );
    }

    #[test]
    fn aliases_anthropic_tool_use_names_before_forwarding() {
        let req = AnthropicMessagesRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![AnthropicMessage {
                role: "assistant".to_string(),
                content: AnthropicContent::Blocks(vec![AnthropicContentBlock::ToolUse {
                    id: "toolu_1".to_string(),
                    name: "ReadFile".to_string(),
                    input: json!({"path":"README.md"}),
                }]),
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: Some(false),
            thinking: None,
        };
        let bridge = ResolvedSkillContext {
            id: "code-review".to_string(),
            version: "1.0.0".to_string(),
            marker: "skill-bridge:code-review@1.0.0".to_string(),
            codex_instructions: "# Review".to_string(),
            references: vec![],
            merge_mode: SkillMergeMode::Prepend,
            tool_aliases: HashMap::from([("ReadFile".to_string(), "read_file".to_string())]),
        };

        let out = translate_anthropic_to_codex(&req, Some(&bridge));
        assert!(out.input.iter().any(|item| matches!(
            item,
            CodexInputItem::FunctionCall { name, .. } if name == "read_file"
        )));
    }

    #[test]
    fn maps_high_reasoning_model_alias_to_base_model() {
        let req = AnthropicMessagesRequest {
            model: "gpt-5.2-codex-high".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("hi".to_string()),
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: Some(false),
            thinking: None,
        };

        let out = translate_anthropic_to_codex(&req, None);

        assert_eq!(out.model, "gpt-5.2-codex");
        assert_eq!(out.reasoning, Some(json!({
            "summary": "auto",
            "effort": "high"
        })));
    }

    #[test]
    fn preserves_explicit_thinking_budget_over_model_alias() {
        let req = AnthropicMessagesRequest {
            model: "gpt-5.2-codex-xhigh".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("hi".to_string()),
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: Some(false),
            thinking: Some(json!({"budget_tokens": 1000})),
        };

        let out = translate_anthropic_to_codex(&req, None);

        assert_eq!(out.model, "gpt-5.2-codex");
        assert_eq!(out.reasoning, Some(json!({
            "summary": "auto",
            "effort": "low"
        })));
    }
}
