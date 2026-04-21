use serde::Serialize;

use crate::domain::anthropic::{AnthropicContent, AnthropicContentBlock, AnthropicMessagesRequest};
use crate::domain::openai::{ChatCompletionsRequest, OpenAIContent};

use super::registry::SurfaceRegistry;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifiedSurface {
    pub requested_name: String,
    pub surface_id: Option<String>,
    pub kind: String,
    pub known: bool,
}

#[derive(Debug, Clone)]
pub struct SurfaceClassifier {
    registry: SurfaceRegistry,
}

impl SurfaceClassifier {
    pub fn new(registry: SurfaceRegistry) -> Self {
        Self { registry }
    }

    pub fn classify_anthropic_request(
        &self,
        request: &AnthropicMessagesRequest,
    ) -> Vec<ClassifiedSurface> {
        let mut surfaces = Vec::new();

        if let Some(tools) = &request.tools {
            for tool in tools {
                if let Some(name) = tool.name.as_deref() {
                    surfaces.push(self.classify_name(name, "tool"));
                }
            }
        }

        for command in anthropic_commands(request) {
            surfaces.push(self.classify_name(&command, "command"));
        }

        dedupe(surfaces)
    }

    pub fn classify_openai_request(
        &self,
        request: &ChatCompletionsRequest,
    ) -> Vec<ClassifiedSurface> {
        let mut surfaces = Vec::new();

        if let Some(tools) = &request.tools {
            for tool in tools {
                surfaces.push(self.classify_name(&tool.function.name, "tool"));
            }
        }

        if let Some(functions) = &request.functions {
            for function in functions {
                surfaces.push(self.classify_name(&function.name, "tool"));
            }
        }

        for command in openai_commands(request) {
            surfaces.push(self.classify_name(&command, "command"));
        }

        dedupe(surfaces)
    }

    pub fn classify_name(&self, name: &str, kind: &str) -> ClassifiedSurface {
        let descriptor = self.registry.find_by_source_name(name);
        ClassifiedSurface {
            requested_name: name.to_string(),
            surface_id: descriptor.map(|surface| surface.id.clone()),
            kind: kind.to_string(),
            known: descriptor.is_some(),
        }
    }
}

fn anthropic_commands(request: &AnthropicMessagesRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter(|message| message.role == "user")
        .flat_map(|message| match &message.content {
            AnthropicContent::Text(text) => first_command(text).into_iter().collect::<Vec<_>>(),
            AnthropicContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    AnthropicContentBlock::Text { text } => first_command(text),
                    _ => None,
                })
                .collect(),
        })
        .collect()
}

fn openai_commands(request: &ChatCompletionsRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter(|message| message.role == "user")
        .flat_map(|message| match &message.content {
            Some(OpenAIContent::Text(text)) => first_command(text).into_iter().collect::<Vec<_>>(),
            Some(OpenAIContent::Parts(parts)) => parts
                .iter()
                .filter_map(|part| match part {
                    crate::domain::openai::OpenAIContentPart::Text { text } => {
                        text.as_deref().and_then(first_command)
                    }
                    _ => None,
                })
                .collect(),
            None => Vec::new(),
        })
        .collect()
}

fn first_command(text: &str) -> Option<String> {
    let token = text.split_whitespace().next()?;
    token.starts_with('/').then(|| token.to_string())
}

fn dedupe(items: Vec<ClassifiedSurface>) -> Vec<ClassifiedSurface> {
    let mut out = Vec::new();
    for item in items {
        if !out.iter().any(|existing: &ClassifiedSurface| {
            existing
                .requested_name
                .eq_ignore_ascii_case(&item.requested_name)
        }) {
            out.push(item);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn classifier_identifies_known_and_unknown_surfaces() {
        let classifier = SurfaceClassifier::new(SurfaceRegistry::new());
        let anthropic: AnthropicMessagesRequest = serde_json::from_value(json!({
            "model": "gpt-5.4",
            "tools": [
                { "name": "Read" },
                { "name": "TaskCreate" },
                { "name": "UnknownTool" }
            ],
            "messages": [
                { "role": "user", "content": "/plan build the feature" }
            ]
        }))
        .expect("request");

        let surfaces = classifier.classify_anthropic_request(&anthropic);
        assert!(surfaces
            .iter()
            .any(|surface| surface.surface_id.as_deref() == Some("tool.read")));
        assert!(surfaces
            .iter()
            .any(|surface| surface.surface_id.as_deref() == Some("tool.taskcreate")));
        assert!(surfaces
            .iter()
            .any(|surface| surface.surface_id.as_deref() == Some("command.plan")));
        assert!(surfaces.iter().any(|surface| !surface.known));
    }
}
