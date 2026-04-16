use crate::domain::anthropic::{
    AnthropicContent, AnthropicContentBlock, AnthropicMessage, AnthropicMessagesRequest,
    AnthropicSystem, AnthropicSystemBlock,
};
use crate::skills::loader::{load_codex_instructions, load_reference_payloads};
use crate::skills::{ResolvedSkillContext, SkillRegistry};

pub const SKILL_MARKER_PREFIX: &str = "skill-bridge:";

#[derive(Debug, Clone)]
pub struct PreparedAnthropicRequest {
    pub request: AnthropicMessagesRequest,
    pub requested_marker: Option<String>,
    pub bridge: Option<ResolvedSkillContext>,
}

#[derive(Debug, Default)]
struct MarkerScan {
    marker: Option<String>,
    invalid_line: Option<String>,
}

pub fn prepare_anthropic_request(
    request: &AnthropicMessagesRequest,
    registry: Option<&SkillRegistry>,
    trace_id: &str,
) -> PreparedAnthropicRequest {
    let marker_scan = scan_request_for_marker(request);
    if let Some(invalid_line) = marker_scan.invalid_line.as_deref() {
        log::warn!("[{trace_id}] malformed skill marker ignored line={invalid_line}");
    }
    let requested_marker = marker_scan.marker;
    let sanitized_request = strip_marker_from_request(request);

    let bridge = requested_marker.as_ref().and_then(|marker| {
        let registry = registry?;
        let skill = registry.resolve_marker(marker)?;

        match load_codex_instructions(&skill.codex_artifact_path) {
            Ok(codex_instructions) => {
                let references = match skill.reference_bundle_path.as_ref() {
                    Some(reference_bundle_path) => match load_reference_payloads(reference_bundle_path)
                    {
                        Ok(references) => references,
                        Err(err) => {
                            log::warn!(
                                "[{trace_id}] failed to load reference bundle for marker={marker}: {err}"
                            );
                            Vec::new()
                        }
                    },
                    None => Vec::new(),
                };

                Some(ResolvedSkillContext {
                    id: skill.id.clone(),
                    version: skill.version.clone(),
                    marker: skill.marker.clone(),
                    codex_instructions,
                    references,
                    merge_mode: skill.merge_mode,
                    tool_aliases: skill.tool_aliases.clone(),
                })
            }
            Err(err) => {
                log::warn!("[{trace_id}] failed to load codex artifact for marker={marker}: {err}");
                None
            }
        }
    });

    PreparedAnthropicRequest {
        request: sanitized_request,
        requested_marker,
        bridge,
    }
}

fn scan_request_for_marker(request: &AnthropicMessagesRequest) -> MarkerScan {
    if let Some(system) = &request.system {
        match system {
            AnthropicSystem::Text(text) => {
                let scan = scan_text_for_marker(text);
                if scan.marker.is_some() {
                    return scan;
                }
                if scan.invalid_line.is_some() {
                    return scan;
                }
            }
            AnthropicSystem::Blocks(blocks) => {
                for block in blocks {
                    if let Some(text) = &block.text {
                        let scan = scan_text_for_marker(text);
                        if scan.marker.is_some() || scan.invalid_line.is_some() {
                            return scan;
                        }
                    }
                }
            }
        }
    }

    for message in &request.messages {
        if message.role != "system" {
            continue;
        }

        match &message.content {
            AnthropicContent::Text(text) => {
                let scan = scan_text_for_marker(text);
                if scan.marker.is_some() || scan.invalid_line.is_some() {
                    return scan;
                }
            }
            AnthropicContent::Blocks(blocks) => {
                for block in blocks {
                    if let AnthropicContentBlock::Text { text } = block {
                        let scan = scan_text_for_marker(text);
                        if scan.marker.is_some() || scan.invalid_line.is_some() {
                            return scan;
                        }
                    }
                }
            }
        }
    }

    MarkerScan::default()
}

fn scan_text_for_marker(text: &str) -> MarkerScan {
    let mut scan = MarkerScan::default();

    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with(SKILL_MARKER_PREFIX) {
            continue;
        }

        let candidate = trimmed
            .split_whitespace()
            .next()
            .map(|value| value.to_string());

        match candidate {
            Some(marker) if is_valid_marker(&marker) => {
                scan.marker = Some(marker);
                return scan;
            }
            Some(_) => {
                if scan.invalid_line.is_none() {
                    scan.invalid_line = Some(trimmed.to_string());
                }
            }
            None => {}
        }
    }

    scan
}

fn is_valid_marker(marker: &str) -> bool {
    if !marker.starts_with(SKILL_MARKER_PREFIX) {
        return false;
    }

    let suffix = marker.trim_start_matches(SKILL_MARKER_PREFIX);
    let Some((skill_id, version)) = suffix.split_once('@') else {
        return false;
    };

    !skill_id.trim().is_empty() && !version.trim().is_empty()
}

fn strip_marker_from_request(request: &AnthropicMessagesRequest) -> AnthropicMessagesRequest {
    let mut sanitized = request.clone();

    sanitized.system = sanitized.system.as_ref().and_then(strip_marker_from_system);
    sanitized.messages = sanitized
        .messages
        .iter()
        .map(strip_marker_from_message)
        .collect();

    sanitized
}

fn strip_marker_from_system(system: &AnthropicSystem) -> Option<AnthropicSystem> {
    match system {
        AnthropicSystem::Text(text) => {
            let text = strip_marker_from_text(text);
            if text.is_empty() {
                None
            } else {
                Some(AnthropicSystem::Text(text))
            }
        }
        AnthropicSystem::Blocks(blocks) => {
            let blocks = blocks
                .iter()
                .filter_map(|block| {
                    let text = block.text.as_deref()?;
                    let sanitized = strip_marker_from_text(text);
                    if sanitized.is_empty() {
                        None
                    } else {
                        Some(AnthropicSystemBlock {
                            text: Some(sanitized),
                        })
                    }
                })
                .collect::<Vec<_>>();

            if blocks.is_empty() {
                None
            } else {
                Some(AnthropicSystem::Blocks(blocks))
            }
        }
    }
}

fn strip_marker_from_message(message: &AnthropicMessage) -> AnthropicMessage {
    if message.role != "system" {
        return message.clone();
    }

    let content = match &message.content {
        AnthropicContent::Text(text) => AnthropicContent::Text(strip_marker_from_text(text)),
        AnthropicContent::Blocks(blocks) => AnthropicContent::Blocks(
            blocks
                .iter()
                .filter_map(|block| match block {
                    AnthropicContentBlock::Text { text } => {
                        let sanitized = strip_marker_from_text(text);
                        if sanitized.is_empty() {
                            None
                        } else {
                            Some(AnthropicContentBlock::Text { text: sanitized })
                        }
                    }
                    _ => Some(block.clone()),
                })
                .collect(),
        ),
    };

    AnthropicMessage {
        role: message.role.clone(),
        content,
    }
}

fn strip_marker_from_text(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim().starts_with(SKILL_MARKER_PREFIX))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::domain::anthropic::AnthropicSystem;
    use crate::skills::loader::load_skill_registry;

    fn fixture_registry_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skill_bridge/registry.json")
    }

    #[test]
    fn prepares_request_and_strips_marker_from_top_level_system() {
        let registry = load_skill_registry(fixture_registry_path()).expect("registry");
        let request = AnthropicMessagesRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("review the diff".to_string()),
            }],
            system: Some(AnthropicSystem::Text(
                "skill-bridge:code-review@1.0.0\nUse a strict rubric.".to_string(),
            )),
            tools: None,
            tool_choice: None,
            stream: Some(false),
            thinking: None,
        };

        let prepared = prepare_anthropic_request(&request, Some(&registry), "test");
        assert_eq!(
            prepared.requested_marker.as_deref(),
            Some("skill-bridge:code-review@1.0.0")
        );
        assert!(prepared.bridge.is_some());
        assert_eq!(
            prepared
                .bridge
                .as_ref()
                .map(|bridge| bridge.references.len()),
            Some(1)
        );
        match &prepared.request.system {
            Some(AnthropicSystem::Text(text)) => assert_eq!(text, "Use a strict rubric."),
            _ => panic!("expected sanitized top-level system text"),
        }
    }

    #[test]
    fn prepares_request_from_system_message_blocks() {
        let request = AnthropicMessagesRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![AnthropicMessage {
                role: "system".to_string(),
                content: AnthropicContent::Blocks(vec![
                    AnthropicContentBlock::Text {
                        text: "skill-bridge:code-review@1.0.0".to_string(),
                    },
                    AnthropicContentBlock::Text {
                        text: "Review for correctness.".to_string(),
                    },
                ]),
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: Some(false),
            thinking: Some(json!({"budget_tokens": 5000})),
        };

        let prepared = prepare_anthropic_request(&request, None, "test");
        assert_eq!(
            prepared.requested_marker.as_deref(),
            Some("skill-bridge:code-review@1.0.0")
        );
        assert!(prepared.bridge.is_none());

        match &prepared.request.messages[0].content {
            AnthropicContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 1);
                assert!(matches!(
                    &blocks[0],
                    AnthropicContentBlock::Text { text } if text == "Review for correctness."
                ));
            }
            _ => panic!("expected blocks"),
        }
    }

    #[test]
    fn ignores_malformed_marker_and_keeps_request_running() {
        let request = AnthropicMessagesRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("continue".to_string()),
            }],
            system: Some(AnthropicSystem::Text(
                "skill-bridge:\nUse a strict rubric.".to_string(),
            )),
            tools: None,
            tool_choice: None,
            stream: Some(false),
            thinking: None,
        };

        let prepared = prepare_anthropic_request(&request, None, "test");
        assert!(prepared.requested_marker.is_none());
        assert!(prepared.bridge.is_none());
        match &prepared.request.system {
            Some(AnthropicSystem::Text(text)) => assert_eq!(text, "Use a strict rubric."),
            _ => panic!("expected sanitized system text"),
        }
    }

    #[test]
    fn falls_back_cleanly_when_codex_artifact_is_missing() {
        use std::fs;

        let temp_dir =
            std::env::temp_dir().join(format!("skill-bridge-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).expect("temp dir");
        let registry_path = temp_dir.join("registry.json");
        fs::write(
            &registry_path,
            r#"{
  "version": "1",
  "skills": [
    {
      "id": "code-review",
      "version": "1.0.0",
      "marker": "skill-bridge:code-review@1.0.0",
      "codex_artifact_path": "missing/SKILL.md",
      "merge_mode": "prepend",
      "tool_aliases": {},
      "compatibility": {
        "anthropic": true,
        "codex": true
      }
    }
  ]
}"#,
        )
        .expect("registry");

        let registry =
            crate::skills::loader::load_skill_registry(&registry_path).expect("registry");
        let request = AnthropicMessagesRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("continue".to_string()),
            }],
            system: Some(AnthropicSystem::Text(
                "skill-bridge:code-review@1.0.0".to_string(),
            )),
            tools: None,
            tool_choice: None,
            stream: Some(false),
            thinking: None,
        };

        let prepared = prepare_anthropic_request(&request, Some(&registry), "test");
        assert_eq!(
            prepared.requested_marker.as_deref(),
            Some("skill-bridge:code-review@1.0.0")
        );
        assert!(prepared.bridge.is_none());
        assert!(prepared.request.system.is_none());

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
