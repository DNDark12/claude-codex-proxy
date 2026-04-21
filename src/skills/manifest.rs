use std::{collections::HashMap, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillMergeMode {
    Prepend,
    Append,
    Replace,
}

impl SkillMergeMode {
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(|v| v.trim().to_ascii_lowercase()) {
            Some(v) if v == "append" => Self::Append,
            Some(v) if v == "replace" => Self::Replace,
            _ => Self::Prepend,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SkillDefinition {
    pub id: String,
    pub version: String,
    pub marker: String,
    pub codex_artifact_path: PathBuf,
    pub reference_bundle_path: Option<PathBuf>,
    pub merge_mode: SkillMergeMode,
    pub tool_aliases: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedSkillContext {
    pub id: String,
    pub version: String,
    pub marker: String,
    pub codex_instructions: String,
    pub references: Vec<ReferencePayload>,
    pub merge_mode: SkillMergeMode,
    pub tool_aliases: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ReferencePayload {
    pub path: String,
    pub content: String,
}
