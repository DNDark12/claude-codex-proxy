use serde::{Deserialize, Serialize};

use crate::jobs::model::{JobKind, JobRecord, JobStatus, SchedulerMode, SchedulingSurface};
use crate::jobs::registry::JobRegistry;
use crate::mapping::tools::ToolWarning;
use crate::surfaces::model::MappingStrategy;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronCreateRequest {
    pub schedule: String,
    pub prompt: String,
    pub durable: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CronCreateResult {
    pub job_id: String,
    pub strategy: MappingStrategy,
    pub scheduler_mode: SchedulingSurface,
    pub warnings: Vec<ToolWarning>,
}

/// CronCreate mapping — SessionCron by default; DurableAutomation with warning (P5-003).
pub async fn map_cron_create(
    request: CronCreateRequest,
    session_id: &str,
    registry: &JobRegistry,
) -> CronCreateResult {
    let durable = request.durable.unwrap_or(false);
    let job_id = format!("cron-{}", simple_id());

    let (kind, mode, surface, warnings) = if durable {
        (
            JobKind::DurableAutomation,
            Some(SchedulerMode::DurableAutomation {
                automation_id: job_id.clone(),
            }),
            SchedulingSurface::DurableRoutine,
            vec![ToolWarning {
                surface_id: "tool.cron_create".to_string(),
                warning: "DurableAutomation: semantics differ from Claude session cron. This entry persists beyond session lifetime.".to_string(),
            }],
        )
    } else {
        (
            JobKind::SessionCron,
            Some(SchedulerMode::SessionCron {
                session_id: session_id.to_string(),
            }),
            SchedulingSurface::SessionCron,
            Vec::new(),
        )
    };

    let job = JobRecord {
        job_id: job_id.clone(),
        origin_surface_id: "tool.cron_create".to_string(),
        kind,
        status: JobStatus::Running,
        scheduler_mode: mode,
        codex_thread_id: None,
        codex_agent_ids: Vec::new(),
        worktree_path: None,
        result_summary: Some(format!("schedule={} prompt={}", request.schedule, request.prompt)),
        warnings: warnings.iter().map(|w| w.warning.clone()).collect(),
    };
    registry.insert(job).await;

    CronCreateResult {
        job_id,
        strategy: MappingStrategy::MediatedNative,
        scheduler_mode: surface,
        warnings,
    }
}

pub async fn map_cron_list(registry: &JobRegistry) -> Vec<JobRecord> {
    registry
        .list()
        .await
        .into_iter()
        .filter(|j| matches!(j.kind, JobKind::SessionCron | JobKind::DurableAutomation))
        .collect()
}

pub async fn map_cron_delete(job_id: &str, registry: &JobRegistry) -> Option<JobRecord> {
    if let Some(mut job) = registry.get(job_id).await {
        job.status = JobStatus::Cancelled;
        registry.insert(job.clone()).await;
        Some(job)
    } else {
        None
    }
}

fn simple_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:032x}", nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    // P5-T01: CronCreate with SessionCron — entry ephemeral
    #[tokio::test]
    async fn cron_create_session_scoped() {
        let registry = JobRegistry::default();
        let result = map_cron_create(
            CronCreateRequest { schedule: "*/5 * * * *".to_string(), prompt: "check".to_string(), durable: None },
            "sess-1",
            &registry,
        ).await;
        assert_eq!(result.scheduler_mode, SchedulingSurface::SessionCron);
        assert!(result.warnings.is_empty());
    }

    // P5-T02: CronCreate with DurableAutomation — warning emitted
    #[tokio::test]
    async fn cron_create_durable_warns() {
        let registry = JobRegistry::default();
        let result = map_cron_create(
            CronCreateRequest { schedule: "0 * * * *".to_string(), prompt: "deploy".to_string(), durable: Some(true) },
            "sess-1",
            &registry,
        ).await;
        assert_eq!(result.scheduler_mode, SchedulingSurface::DurableRoutine);
        assert!(!result.warnings.is_empty());
        assert!(result.warnings[0].warning.contains("DurableAutomation"));
    }

    #[tokio::test]
    async fn cron_list_and_delete() {
        let registry = JobRegistry::default();
        let result = map_cron_create(
            CronCreateRequest { schedule: "* * * * *".to_string(), prompt: "x".to_string(), durable: None },
            "sess-1",
            &registry,
        ).await;
        assert_eq!(map_cron_list(&registry).await.len(), 1);
        let deleted = map_cron_delete(&result.job_id, &registry).await.unwrap();
        assert_eq!(deleted.status, JobStatus::Cancelled);
    }
}
