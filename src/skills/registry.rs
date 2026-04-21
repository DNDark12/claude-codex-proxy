use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use serde::Deserialize;

use crate::skills::manifest::{SkillDefinition, SkillMergeMode};

#[derive(Debug, Clone)]
pub struct SkillRegistry {
    pub version: String,
    skills_by_marker: HashMap<String, SkillDefinition>,
}

impl SkillRegistry {
    pub fn from_raw(raw: RawSkillRegistry, base_dir: &Path) -> Result<Self> {
        let mut skills_by_marker = HashMap::new();

        for raw_skill in raw.skills {
            let marker = raw_skill.marker.trim();
            if marker.is_empty() {
                return Err(anyhow!("skill registry entry is missing marker"));
            }

            let artifact_path = resolve_artifact_path(base_dir, &raw_skill.codex_artifact_path);
            let skill = SkillDefinition {
                id: raw_skill.id,
                version: raw_skill.version,
                marker: marker.to_string(),
                codex_artifact_path: artifact_path,
                reference_bundle_path: raw_skill
                    .reference_bundle_path
                    .as_deref()
                    .map(|path| resolve_artifact_path(base_dir, path)),
                merge_mode: SkillMergeMode::parse(raw_skill.merge_mode.as_deref()),
                tool_aliases: raw_skill.tool_aliases,
            };

            if skills_by_marker
                .insert(skill.marker.clone(), skill)
                .is_some()
            {
                return Err(anyhow!(
                    "duplicate skill marker found in registry: {marker}"
                ));
            }
        }

        Ok(Self {
            version: raw.version.unwrap_or_else(|| "1".to_string()),
            skills_by_marker,
        })
    }

    pub fn resolve_marker(&self, marker: &str) -> Option<&SkillDefinition> {
        self.skills_by_marker.get(marker)
    }

    pub fn len(&self) -> usize {
        self.skills_by_marker.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills_by_marker.is_empty()
    }
}

fn resolve_artifact_path(base_dir: &Path, artifact_path: &str) -> PathBuf {
    let artifact_path = PathBuf::from(artifact_path);
    if artifact_path.is_absolute() {
        return artifact_path;
    }

    base_dir.join(artifact_path)
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawSkillRegistry {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub skills: Vec<RawSkillDefinition>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawSkillDefinition {
    pub id: String,
    pub version: String,
    pub marker: String,
    pub codex_artifact_path: String,
    #[serde(default)]
    pub reference_bundle_path: Option<String>,
    #[serde(default)]
    pub merge_mode: Option<String>,
    #[serde(default)]
    pub tool_aliases: HashMap<String, String>,
    #[serde(default)]
    pub _compatibility: HashMap<String, bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_artifact_path_against_registry_dir() {
        let raw = RawSkillRegistry {
            version: Some("1".to_string()),
            skills: vec![RawSkillDefinition {
                id: "code-review".to_string(),
                version: "1.0.0".to_string(),
                marker: "skill-bridge:code-review@1.0.0".to_string(),
                codex_artifact_path: "skills/code-review/SKILL.md".to_string(),
                reference_bundle_path: Some("skills/code-review/references.json".to_string()),
                merge_mode: Some("prepend".to_string()),
                tool_aliases: HashMap::new(),
                _compatibility: HashMap::new(),
            }],
        };

        let registry = SkillRegistry::from_raw(raw, Path::new("/tmp/bridge")).expect("registry");
        let skill = registry
            .resolve_marker("skill-bridge:code-review@1.0.0")
            .expect("skill");

        assert_eq!(
            skill.codex_artifact_path,
            Path::new("/tmp/bridge/skills/code-review/SKILL.md")
        );
        assert_eq!(
            skill.reference_bundle_path.as_deref(),
            Some(Path::new("/tmp/bridge/skills/code-review/references.json"))
        );
    }
}
