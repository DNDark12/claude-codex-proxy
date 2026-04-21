use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app_server::thread::BridgeThread;
use crate::surfaces::model::MappingStrategy;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolWarning {
    pub surface_id: String,
    pub warning: String,
}

/// Unified result for any tool mapping.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolMappingResult {
    pub surface_id: String,
    pub strategy: MappingStrategy,
    pub params: Value,
    pub warnings: Vec<ToolWarning>,
}

pub fn resolve_thread_path(thread: &BridgeThread, raw_path: &str) -> PathBuf {
    let path = Path::new(raw_path);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    Path::new(&thread.cwd).join(path)
}

pub fn mediated_native_warnings(surface_id: &str) -> Vec<ToolWarning> {
    match surface_id {
        "tool.multiedit" => vec![ToolWarning {
            surface_id: surface_id.to_string(),
            warning: "Atomicity is bridge-mediated; upstream app-server does not guarantee a single filesystem transaction.".to_string(),
        }],
        "tool.write" | "tool.edit" | "tool.bash" => vec![ToolWarning {
            surface_id: surface_id.to_string(),
            warning: "Side-effect surface requires approval-aware execution path.".to_string(),
        }],
        _ => Vec::new(),
    }
}

// === Tier 0: Core Tools ===

/// Read mapping — native (P2-001).
pub fn map_read(thread: &BridgeThread, path: &str) -> ToolMappingResult {
    let resolved = resolve_thread_path(thread, path);
    ToolMappingResult {
        surface_id: "tool.read".to_string(),
        strategy: MappingStrategy::Native,
        params: serde_json::json!({ "path": resolved.to_string_lossy() }),
        warnings: Vec::new(),
    }
}

/// Write mapping — mediated_native + approval (P2-002).
pub fn map_write(thread: &BridgeThread, path: &str, content: &str) -> ToolMappingResult {
    let resolved = resolve_thread_path(thread, path);
    ToolMappingResult {
        surface_id: "tool.write".to_string(),
        strategy: MappingStrategy::MediatedNative,
        params: serde_json::json!({ "path": resolved.to_string_lossy(), "content": content }),
        warnings: mediated_native_warnings("tool.write"),
    }
}

/// Edit mapping — mediated_native + protected-path check (P2-003).
pub fn map_edit(thread: &BridgeThread, path: &str, edits: Value) -> ToolMappingResult {
    let resolved = resolve_thread_path(thread, path);
    let mut warnings = mediated_native_warnings("tool.edit");
    if is_protected_path(&resolved) {
        warnings.push(ToolWarning {
            surface_id: "tool.edit".to_string(),
            warning: format!("Protected path: {}", resolved.display()),
        });
    }
    ToolMappingResult {
        surface_id: "tool.edit".to_string(),
        strategy: MappingStrategy::MediatedNative,
        params: serde_json::json!({ "path": resolved.to_string_lossy(), "edits": edits }),
        warnings,
    }
}

/// MultiEdit mapping + atomicity warning (P2-004).
pub fn map_multiedit(thread: &BridgeThread, edits: Vec<Value>) -> ToolMappingResult {
    ToolMappingResult {
        surface_id: "tool.multiedit".to_string(),
        strategy: MappingStrategy::MediatedNative,
        params: serde_json::json!({ "edits": edits, "cwd": thread.cwd }),
        warnings: mediated_native_warnings("tool.multiedit"),
    }
}

/// Glob mapping — native (P2-005).
pub fn map_glob(thread: &BridgeThread, pattern: &str) -> ToolMappingResult {
    ToolMappingResult {
        surface_id: "tool.glob".to_string(),
        strategy: MappingStrategy::Native,
        params: serde_json::json!({ "pattern": pattern, "cwd": thread.cwd }),
        warnings: Vec::new(),
    }
}

/// Grep mapping — native (P2-006).
pub fn map_grep(thread: &BridgeThread, pattern: &str, path: Option<&str>) -> ToolMappingResult {
    ToolMappingResult {
        surface_id: "tool.grep".to_string(),
        strategy: MappingStrategy::Native,
        params: serde_json::json!({ "pattern": pattern, "path": path, "cwd": thread.cwd }),
        warnings: Vec::new(),
    }
}

/// LS mapping — native (P2-007).
pub fn map_ls(thread: &BridgeThread, path: Option<&str>) -> ToolMappingResult {
    let resolved = path.map(|p| resolve_thread_path(thread, p).to_string_lossy().to_string());
    ToolMappingResult {
        surface_id: "tool.ls".to_string(),
        strategy: MappingStrategy::Native,
        params: serde_json::json!({ "path": resolved.as_deref().unwrap_or(&thread.cwd) }),
        warnings: Vec::new(),
    }
}

/// Bash mapping — mediated_native + approval + cwd continuity (P2-008).
pub fn map_bash(thread: &BridgeThread, command: &str) -> ToolMappingResult {
    ToolMappingResult {
        surface_id: "tool.bash".to_string(),
        strategy: MappingStrategy::MediatedNative,
        params: serde_json::json!({ "command": command, "cwd": thread.cwd }),
        warnings: mediated_native_warnings("tool.bash"),
    }
}

// === Tier 3: Intelligence + Web ===

/// ToolSearch mapping — mediated_native, partial emulation (P5-010).
pub fn map_tool_search(query: &str) -> ToolMappingResult {
    ToolMappingResult {
        surface_id: "tool.tool_search".to_string(),
        strategy: MappingStrategy::MediatedNative,
        params: serde_json::json!({ "query": query }),
        warnings: vec![ToolWarning {
            surface_id: "tool.tool_search".to_string(),
            warning: "Not true parity; only partial discovery/loading emulation. Claude ToolSearch does deferred tool loading, not just listing.".to_string(),
        }],
    }
}

/// WebFetch mapping (P5-011).
pub fn map_web_fetch(url: &str) -> ToolMappingResult {
    ToolMappingResult {
        surface_id: "tool.web_fetch".to_string(),
        strategy: MappingStrategy::MediatedNative,
        params: serde_json::json!({ "url": url }),
        warnings: Vec::new(),
    }
}

/// WebSearch mapping — mediated_native (P5-012).
pub fn map_web_search(query: &str) -> ToolMappingResult {
    ToolMappingResult {
        surface_id: "tool.web_search".to_string(),
        strategy: MappingStrategy::MediatedNative,
        params: serde_json::json!({ "query": query }),
        warnings: Vec::new(),
    }
}

/// Monitor mapping — workflow_emulated (P5-013).
pub fn map_monitor(target: &str) -> ToolMappingResult {
    ToolMappingResult {
        surface_id: "tool.monitor".to_string(),
        strategy: MappingStrategy::WorkflowEmulated,
        params: serde_json::json!({ "target": target }),
        warnings: vec![ToolWarning {
            surface_id: "tool.monitor".to_string(),
            warning: "Emulated via polling/event subscription.".to_string(),
        }],
    }
}

// === Tier 4: Notebook ===

/// NotebookRead mapping — mediated_native (P6-006).
pub fn map_notebook_read(path: &str) -> ToolMappingResult {
    ToolMappingResult {
        surface_id: "tool.notebook_read".to_string(),
        strategy: MappingStrategy::MediatedNative,
        params: serde_json::json!({ "path": path }),
        warnings: Vec::new(),
    }
}

/// NotebookEdit mapping — unsupported_explicit (P6-007).
pub fn map_notebook_edit() -> ToolMappingResult {
    ToolMappingResult {
        surface_id: "tool.notebook_edit".to_string(),
        strategy: MappingStrategy::UnsupportedExplicit,
        params: serde_json::json!(null),
        warnings: vec![ToolWarning {
            surface_id: "tool.notebook_edit".to_string(),
            warning: "Cell-level parity unlikely. NotebookEdit is unsupported.".to_string(),
        }],
    }
}

// === Helpers ===

fn is_protected_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/.git/") || s.ends_with("/.git") || s.contains("/node_modules/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::thread::BridgeThread;
    use crate::mapping::approvals::{ApprovalPolicy, SandboxConfig};

    fn test_thread() -> BridgeThread {
        BridgeThread {
            thread_id: "thread".to_string(),
            bridge_session_id: "session".to_string(),
            cwd: "/tmp/project".to_string(),
            project_root: None,
            approval_policy: ApprovalPolicy::OnRequest,
            sandbox_config: SandboxConfig::WorkspaceWrite,
            created_at_unix: 0,
            turn_count: 0,
        }
    }

    #[test]
    fn resolves_relative_path_from_thread_cwd() {
        let thread = test_thread();
        assert_eq!(
            resolve_thread_path(&thread, "src/main.rs"),
            PathBuf::from("/tmp/project/src/main.rs")
        );
    }

    #[test]
    fn map_read_is_native() {
        let result = map_read(&test_thread(), "src/main.rs");
        assert_eq!(result.strategy, MappingStrategy::Native);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn map_write_has_warnings() {
        let result = map_write(&test_thread(), "out.txt", "hello");
        assert_eq!(result.strategy, MappingStrategy::MediatedNative);
        assert!(!result.warnings.is_empty());
    }

    #[test]
    fn map_bash_has_cwd() {
        let result = map_bash(&test_thread(), "ls -la");
        assert_eq!(result.params["cwd"], "/tmp/project");
    }

    #[test]
    fn map_multiedit_warns_atomicity() {
        let result = map_multiedit(&test_thread(), vec![]);
        assert!(result.warnings.iter().any(|w| w.warning.contains("Atomicity")));
    }

    #[test]
    fn map_edit_warns_protected_path() {
        let result = map_edit(&test_thread(), ".git/config", serde_json::json!({}));
        assert!(result.warnings.iter().any(|w| w.warning.contains("Protected")));
    }

    #[test]
    fn map_notebook_edit_unsupported() {
        let result = map_notebook_edit();
        assert_eq!(result.strategy, MappingStrategy::UnsupportedExplicit);
    }

    #[test]
    fn map_tool_search_warns_partial() {
        let result = map_tool_search("test");
        assert!(result.warnings.iter().any(|w| w.warning.contains("partial")));
    }

    // P5-T04: WebFetch works where runtime exposes it
    #[test]
    fn map_web_fetch_works() {
        let result = map_web_fetch("https://example.com");
        assert_eq!(result.surface_id, "tool.web_fetch");
        assert_eq!(result.strategy, MappingStrategy::MediatedNative);
        assert_eq!(result.params["url"], "https://example.com");
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn map_web_search_works() {
        let result = map_web_search("rust async");
        assert_eq!(result.strategy, MappingStrategy::MediatedNative);
    }

    #[test]
    fn map_glob_is_native() {
        let result = map_glob(&test_thread(), "**/*.rs");
        assert_eq!(result.strategy, MappingStrategy::Native);
    }

    #[test]
    fn map_grep_is_native() {
        let result = map_grep(&test_thread(), "TODO", Some("src/"));
        assert_eq!(result.strategy, MappingStrategy::Native);
    }

    #[test]
    fn map_ls_uses_cwd_when_no_path() {
        let result = map_ls(&test_thread(), None);
        assert_eq!(result.params["path"], "/tmp/project");
    }

    #[test]
    fn map_monitor_is_emulated() {
        let result = map_monitor("build.log");
        assert_eq!(result.strategy, MappingStrategy::WorkflowEmulated);
    }

    #[test]
    fn map_notebook_read_is_mediated() {
        let result = map_notebook_read("notebook.ipynb");
        assert_eq!(result.strategy, MappingStrategy::MediatedNative);
    }
}
