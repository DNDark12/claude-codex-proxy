use std::collections::HashMap;

use serde_json::Value;

use crate::domain::anthropic::{AnthropicTool, AnthropicToolChoice};
use crate::domain::codex::{CodexToolChoice, CodexToolDefinition};
use crate::domain::openai::{OpenAIFunction, OpenAITool};

fn normalize_schema(schema: Value) -> Value {
    if schema.get("type").and_then(Value::as_str) == Some("object")
        && schema.get("properties").is_none()
    {
        let mut obj = schema.as_object().cloned().unwrap_or_default();
        obj.insert("properties".to_string(), Value::Object(Default::default()));
        return Value::Object(obj);
    }
    schema
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

fn normalize_tool_strategy(strategy: &str) -> String {
    match strategy {
        "any" => "required".to_string(),
        "tool" => "required".to_string(),
        other => other.to_string(),
    }
}

fn infer_anthropic_tool_name(tool: &AnthropicTool) -> Option<String> {
    if let Some(name) = tool.name.as_ref().filter(|v| !v.trim().is_empty()) {
        return Some(name.clone());
    }

    let raw_type = tool.tool_type.as_ref()?.trim();
    if raw_type.is_empty() {
        return None;
    }

    // Example: "text_editor_20250124" -> "text_editor"
    let inferred = raw_type
        .rsplit_once('_')
        .and_then(|(prefix, suffix)| {
            if suffix.chars().all(|c| c.is_ascii_digit()) {
                Some(prefix.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| raw_type.to_string());

    if inferred.is_empty() {
        None
    } else {
        Some(inferred)
    }
}

pub fn openai_tools_to_codex(tools: &[OpenAITool]) -> Vec<CodexToolDefinition> {
    tools
        .iter()
        .map(|tool| CodexToolDefinition {
            tool_type: "function".to_string(),
            name: tool.function.name.clone(),
            description: tool.function.description.clone(),
            parameters: tool.function.parameters.clone().map(normalize_schema),
        })
        .collect()
}

pub fn openai_functions_to_codex(functions: &[OpenAIFunction]) -> Vec<CodexToolDefinition> {
    functions
        .iter()
        .map(|f| CodexToolDefinition {
            tool_type: "function".to_string(),
            name: f.name.clone(),
            description: f.description.clone(),
            parameters: f.parameters.clone().map(normalize_schema),
        })
        .collect()
}

pub fn openai_tool_choice_to_codex(choice: &Value) -> Option<CodexToolChoice> {
    if let Some(strategy) = choice.as_str() {
        return Some(CodexToolChoice::Strategy(normalize_tool_strategy(strategy)));
    }

    let obj = choice.as_object()?;
    let choice_type = obj.get("type")?.as_str()?;

    if choice_type == "function" {
        let name = obj
            .get("function")
            .and_then(|v| v.get("name"))
            .and_then(Value::as_str)?;
        return Some(CodexToolChoice::Function {
            choice_type: "function".to_string(),
            name: name.to_string(),
        });
    }

    None
}

pub fn anthropic_tools_to_codex(
    tools: &[AnthropicTool],
    aliases: Option<&HashMap<String, String>>,
) -> Vec<CodexToolDefinition> {
    tools
        .iter()
        .filter_map(|tool| {
            let name = apply_tool_alias(&infer_anthropic_tool_name(tool)?, aliases);
            Some(CodexToolDefinition {
                tool_type: "function".to_string(),
                name,
                description: tool.description.clone(),
                parameters: tool.input_schema.clone().map(normalize_schema),
            })
        })
        .collect()
}

pub fn anthropic_tool_choice_to_codex(
    choice: &AnthropicToolChoice,
    aliases: Option<&HashMap<String, String>>,
) -> Option<CodexToolChoice> {
    match choice {
        AnthropicToolChoice::Simple(v) => {
            Some(CodexToolChoice::Strategy(normalize_tool_strategy(v)))
        }
        AnthropicToolChoice::Object(obj) => match obj.choice_type.as_str() {
            "auto" => Some(CodexToolChoice::Strategy("auto".to_string())),
            "any" => Some(CodexToolChoice::Strategy("required".to_string())),
            "tool" => obj.name.as_ref().map(|name| CodexToolChoice::Function {
                choice_type: "function".to_string(),
                name: apply_tool_alias(name, aliases),
            }),
            "none" => Some(CodexToolChoice::Strategy("none".to_string())),
            other => Some(CodexToolChoice::Strategy(normalize_tool_strategy(other))),
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::domain::anthropic::{AnthropicToolChoice, AnthropicToolChoiceObject};

    #[test]
    fn maps_anthropic_tool_without_name_from_type() {
        let tools = vec![AnthropicTool {
            name: None,
            description: Some("file editor".to_string()),
            input_schema: Some(json!({"type":"object"})),
            tool_type: Some("text_editor_20250124".to_string()),
        }];

        let out = anthropic_tools_to_codex(&tools, None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "text_editor");
    }

    #[test]
    fn maps_simple_any_to_required() {
        let out =
            anthropic_tool_choice_to_codex(&AnthropicToolChoice::Simple("any".to_string()), None);
        match out {
            Some(CodexToolChoice::Strategy(v)) => assert_eq!(v, "required"),
            _ => panic!("unexpected tool choice"),
        }
    }

    #[test]
    fn maps_object_tool_without_name_to_none() {
        let out = anthropic_tool_choice_to_codex(
            &AnthropicToolChoice::Object(AnthropicToolChoiceObject {
                choice_type: "tool".to_string(),
                name: None,
            }),
            None,
        );
        assert!(out.is_none());
    }

    #[test]
    fn applies_tool_aliases_to_anthropic_tool_names() {
        let tools = vec![AnthropicTool {
            name: Some("ReadFile".to_string()),
            description: Some("file reader".to_string()),
            input_schema: Some(json!({"type":"object"})),
            tool_type: None,
        }];
        let aliases = HashMap::from([("ReadFile".to_string(), "read_file".to_string())]);

        let out = anthropic_tools_to_codex(&tools, Some(&aliases));
        assert_eq!(out[0].name, "read_file");
    }
}
