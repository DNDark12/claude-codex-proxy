use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::skills::manifest::ReferencePayload;
use crate::skills::registry::{RawSkillRegistry, SkillRegistry};

pub fn load_skill_registry<P: AsRef<Path>>(path: P) -> Result<SkillRegistry> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read skills registry at {}", path.display()))?;
    let registry: RawSkillRegistry =
        serde_json::from_str(&raw).context("failed to parse skills registry json")?;

    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    SkillRegistry::from_raw(registry, base_dir)
}

pub fn load_codex_instructions<P: AsRef<Path>>(path: P) -> Result<String> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read codex skill artifact at {}", path.display()))?;

    Ok(strip_skill_frontmatter(&raw))
}

pub fn load_reference_payloads<P: AsRef<Path>>(path: P) -> Result<Vec<ReferencePayload>> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read reference bundle at {}", path.display()))?;
    let bundle: RawReferenceBundle =
        serde_json::from_str(&raw).context("failed to parse reference bundle json")?;

    Ok(bundle
        .references
        .into_iter()
        .map(|reference| ReferencePayload {
            path: reference.path,
            content: reference.content,
        })
        .collect())
}

fn strip_skill_frontmatter(raw: &str) -> String {
    let mut lines = raw.lines();
    if lines.next() != Some("---") {
        return raw.trim().to_string();
    }

    let mut body = Vec::new();
    let mut frontmatter_closed = false;
    for line in lines {
        if !frontmatter_closed {
            if line == "---" {
                frontmatter_closed = true;
            }
            continue;
        }
        body.push(line);
    }

    if frontmatter_closed {
        body.join("\n").trim().to_string()
    } else {
        raw.trim().to_string()
    }
}

#[derive(Debug, Deserialize)]
struct RawReferenceBundle {
    #[serde(default)]
    references: Vec<RawReferencePayload>,
}

#[derive(Debug, Deserialize)]
struct RawReferencePayload {
    path: String,
    content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_frontmatter_from_skill_markdown() {
        let raw = r#"---
name: code-review
description: Review code changes
---

# Review

Look for correctness and risk.
"#;

        assert_eq!(
            strip_skill_frontmatter(raw),
            "# Review\n\nLook for correctness and risk."
        );
    }

    #[test]
    fn loads_reference_bundle_json() {
        let bundle = load_reference_payloads(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/skill_bridge/code-review/references.json"),
        )
        .expect("bundle");

        assert_eq!(bundle.len(), 1);
        assert_eq!(bundle[0].path, "references/review-rubric.md");
        assert!(bundle[0].content.contains("correctness issues"));
    }
}
