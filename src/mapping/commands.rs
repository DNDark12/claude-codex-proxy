use serde::Serialize;

use crate::jobs::{registry::JobRegistry, JobExecutor};
use crate::mapping::review::ReviewRequest;
use crate::surfaces::model::MappingStrategy;

/// Result for command mappings that return structured responses.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    pub surface_id: String,
    pub strategy: MappingStrategy,
    pub success: bool,
    pub message: String,
    pub data: serde_json::Value,
}

/// `/tasks` command — list active jobs (P3-008).
pub async fn map_tasks_command(registry: &JobRegistry) -> CommandResult {
    let jobs = registry.list().await;
    CommandResult {
        surface_id: "command.tasks".to_string(),
        strategy: MappingStrategy::MediatedNative,
        success: true,
        message: format!("{} active jobs", jobs.len()),
        data: serde_json::to_value(&jobs).unwrap_or_default(),
    }
}

/// `/security-review` command — trigger security review workflow (P3-025).
pub async fn map_security_review_command(
    request: ReviewRequest,
    executor: Option<&JobExecutor>,
    registry: &JobRegistry,
) -> CommandResult {
    let result = crate::mapping::review::map_security_review(request, executor, registry).await;
    CommandResult {
        surface_id: "command.security_review".to_string(),
        strategy: MappingStrategy::WorkflowEmulated,
        success: true,
        message: format!("Security review started: {}", result.job_id),
        data: serde_json::to_value(&result).unwrap_or_default(),
    }
}

/// `/schedule` command — unsupported_explicit (P5-007).
pub fn map_schedule_command() -> CommandResult {
    CommandResult {
        surface_id: "command.schedule".to_string(),
        strategy: MappingStrategy::UnsupportedExplicit,
        success: false,
        message: "Durable routines (/schedule) are not supported. Claude /schedule runs on Anthropic cloud infrastructure which has no Codex equivalent. Use CronCreate for session-scoped scheduling instead.".to_string(),
        data: serde_json::json!({
            "unsupported_reason": "missing_backend_primitive",
            "alternative": "CronCreate (session-scoped scheduling)"
        }),
    }
}

/// `/mcp` command — read/write Codex config.toml MCP bridge (P6-004).
pub fn map_mcp_command(action: &str) -> CommandResult {
    CommandResult {
        surface_id: "command.mcp".to_string(),
        strategy: MappingStrategy::MediatedNative,
        success: true,
        message: format!("MCP {} via config.toml bridge", action),
        data: serde_json::json!({ "action": action, "config_path": ".codex/config.toml" }),
    }
}

/// `/plugin` command — skill install (P6-005).
pub fn map_plugin_command(plugin_name: &str) -> CommandResult {
    CommandResult {
        surface_id: "command.plugin".to_string(),
        strategy: MappingStrategy::WorkflowEmulated,
        success: true,
        message: format!("Plugin {} normalized to Codex skill model", plugin_name),
        data: serde_json::json!({ "plugin": plugin_name }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // P5-T03: /schedule returns structured unsupported reason
    #[test]
    fn schedule_command_unsupported() {
        let result = map_schedule_command();
        assert_eq!(result.strategy, MappingStrategy::UnsupportedExplicit);
        assert!(!result.success);
        assert!(result.message.contains("not supported"));
        assert_eq!(
            result.data["unsupported_reason"],
            "missing_backend_primitive"
        );
    }

    #[tokio::test]
    async fn tasks_command_lists_jobs() {
        let registry = JobRegistry::default();
        let result = map_tasks_command(&registry).await;
        assert!(result.success);
        assert!(result.message.contains("0 active jobs"));
    }
}
