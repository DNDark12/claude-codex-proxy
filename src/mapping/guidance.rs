use serde::Serialize;

use crate::surfaces::model::MappingStrategy;

/// /init workflow — inspect repo, propose AGENTS.md (P6-001).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuidanceInitResult {
    pub strategy: MappingStrategy,
    pub proposed_path: String,
    pub content_preview: Option<String>,
    pub warnings: Vec<String>,
}

pub fn map_init_guidance(project_root: &str) -> GuidanceInitResult {
    GuidanceInitResult {
        strategy: MappingStrategy::WorkflowEmulated,
        proposed_path: format!("{}/AGENTS.md", project_root),
        content_preview: None,
        warnings: Vec::new(),
    }
}

/// /memory workflow — import from CLAUDE.md, proposal-first, no auto-sync (P6-002).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryImportResult {
    pub strategy: MappingStrategy,
    pub source_path: Option<String>,
    pub target_path: String,
    pub proposal_only: bool,
    pub warnings: Vec<String>,
}

pub fn map_memory_import(project_root: &str) -> MemoryImportResult {
    let claude_md = format!("{}/CLAUDE.md", project_root);
    let agents_md = format!("{}/AGENTS.md", project_root);

    MemoryImportResult {
        strategy: MappingStrategy::WorkflowEmulated,
        source_path: Some(claude_md),
        target_path: agents_md,
        proposal_only: true,
        warnings: vec![
            "Memory import is proposal-only. No auto-sync between Claude memory and Codex guidance."
                .to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // P6-T02: init_guidance_bootstrap — AGENTS.md proposed
    #[test]
    fn init_proposes_agents_md() {
        let result = map_init_guidance("/project");
        assert_eq!(result.proposed_path, "/project/AGENTS.md");
        assert_eq!(result.strategy, MappingStrategy::WorkflowEmulated);
    }

    // P6-T01: memory_import — CLAUDE.md → guidance proposal
    #[test]
    fn memory_import_proposal_only() {
        let result = map_memory_import("/project");
        assert!(result.proposal_only);
        assert_eq!(result.source_path.as_deref(), Some("/project/CLAUDE.md"));
        assert_eq!(result.target_path, "/project/AGENTS.md");
    }

    // P6-T03: No auto-sync
    #[test]
    fn memory_import_warns_no_auto_sync() {
        let result = map_memory_import("/project");
        assert!(result.warnings.iter().any(|w| w.contains("No auto-sync")));
    }
}
