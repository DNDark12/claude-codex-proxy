use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app_server::UserInput;
use crate::app_server::thread::BridgeThread;
use crate::jobs::{ExecutorRequest, JobExecutor};
use crate::jobs::model::{JobKind, JobRecord, JobStatus};
use crate::jobs::registry::JobRegistry;
use crate::mapping::tools::ToolWarning;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCreateRequest {
    pub description: String,
    pub instructions: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCreateResult {
    pub job_id: String,
    pub thread_id: Option<String>,
    pub status: JobStatus,
    pub warnings: Vec<ToolWarning>,
}

pub async fn map_task_create(
    request: TaskCreateRequest,
    thread: &BridgeThread,
    executor: Option<&JobExecutor>,
    registry: &JobRegistry,
) -> TaskCreateResult {
    let task_text = match request.instructions.as_deref() {
        Some(instructions) if !instructions.is_empty() => {
            format!("{}\n\n{}", request.description, instructions)
        }
        _ => request.description.clone(),
    };

    if let Some(executor) = executor {
        if let Ok(start) = executor
            .start_job(ExecutorRequest {
                origin_surface_id: "tool.task_create".to_string(),
                kind: JobKind::Task,
                cwd: request.cwd.unwrap_or_else(|| thread.cwd.clone()),
                model: "gpt-5.4".to_string(),
                developer_instructions: None,
                input: vec![UserInput::Text { text: task_text }],
            })
            .await
        {
            let status = registry
                .get(&start.job_id)
                .await
                .map(|job| job.status)
                .unwrap_or(JobStatus::Running);
            return TaskCreateResult {
                job_id: start.job_id,
                thread_id: Some(start.thread_id),
                status,
                warnings: Vec::new(),
            };
        }
    }

    let job_id = format!("task-{}", uuid_v4());
    let job = JobRecord {
        job_id: job_id.clone(),
        origin_surface_id: "tool.task_create".to_string(),
        kind: JobKind::Task,
        status: JobStatus::Queued,
        scheduler_mode: None,
        codex_thread_id: Some(thread.thread_id.clone()),
        codex_turn_id: None,
        codex_agent_ids: Vec::new(),
        worktree_path: None,
        result_summary: Some(match request.instructions.as_deref() {
            Some(instructions) if !instructions.is_empty() => {
                format!("{} — {}", request.description, instructions)
            }
            _ => request.description.clone(),
        }),
        warnings: Vec::new(),
        error_message: None,
    };
    registry.insert(job).await;

    TaskCreateResult {
        job_id,
        thread_id: Some(thread.thread_id.clone()),
        status: JobStatus::Queued,
        warnings: Vec::new(),
    }
}

pub async fn map_task_get(job_id: &str, registry: &JobRegistry) -> Option<JobRecord> {
    registry.get(job_id).await
}

pub async fn map_task_list(registry: &JobRegistry) -> Vec<JobRecord> {
    registry
        .list()
        .await
        .into_iter()
        .filter(|j| j.kind == JobKind::Task)
        .collect()
}

pub async fn map_task_update(
    job_id: &str,
    update: Value,
    executor: Option<&JobExecutor>,
    registry: &JobRegistry,
) -> Option<JobRecord> {
    if let Some(mut job) = registry.get(job_id).await {
        if let Some(text) = update
            .get("text")
            .or_else(|| update.get("input"))
            .and_then(|v| v.as_str())
        {
            if let Some(executor) = executor {
                if executor.send_input(job_id, text.to_string()).await.is_err() {
                    return None;
                }
            }
            job.status = JobStatus::Running;
            job.result_summary = Some(text.to_string());
        }
        if let Some(summary) = update.get("resultSummary").and_then(|v| v.as_str()) {
            job.result_summary = Some(summary.to_string());
        }
        if let Some(status) = update.get("status").and_then(|v| v.as_str()) {
            job.status = match status {
                "running" => JobStatus::Running,
                "completed" => JobStatus::Completed,
                "failed" => JobStatus::Failed,
                "cancelled" => JobStatus::Cancelled,
                _ => job.status,
            };
        }
        registry.insert(job.clone()).await;
        Some(job)
    } else {
        None
    }
}

pub async fn map_task_stop(
    job_id: &str,
    executor: Option<&JobExecutor>,
    registry: &JobRegistry,
) -> Option<JobRecord> {
    if let Some(mut job) = registry.get(job_id).await {
        if let Some(executor) = executor {
            let _ = executor.interrupt(job_id).await;
        }
        job.status = JobStatus::Cancelled;
        registry.insert(job.clone()).await;
        Some(job)
    } else {
        None
    }
}

fn uuid_v4() -> String {
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
    use crate::app_server::thread::BridgeThread;
    use crate::mapping::approvals::{ApprovalPolicy, SandboxConfig};

    fn test_thread() -> BridgeThread {
        BridgeThread {
            thread_id: "thread-1".to_string(),
            bridge_session_id: "sess-1".to_string(),
            cwd: "/tmp".to_string(),
            project_root: None,
            approval_policy: ApprovalPolicy::OnRequest,
            sandbox_config: SandboxConfig::WorkspaceWrite,
            created_at_unix: 0,
            turn_count: 0,
        }
    }

    // P3-T01: TaskCreate → job created, thread spawned
    #[tokio::test]
    async fn task_create_creates_job() {
        let registry = JobRegistry::default();
        let result = map_task_create(
            TaskCreateRequest { description: "test".to_string(), instructions: None, cwd: None },
            &test_thread(),
            None,
            &registry,
        ).await;
        assert_eq!(result.status, JobStatus::Queued);
        assert!(registry.get(&result.job_id).await.is_some());
    }

    // P3-T02: TaskGet/TaskList reflect registry state
    #[tokio::test]
    async fn task_get_and_list() {
        let registry = JobRegistry::default();
        let result = map_task_create(
            TaskCreateRequest { description: "a".to_string(), instructions: None, cwd: None },
            &test_thread(),
            None,
            &registry,
        ).await;
        assert!(map_task_get(&result.job_id, &registry).await.is_some());
        assert_eq!(map_task_list(&registry).await.len(), 1);
    }

    #[tokio::test]
    async fn task_stop_cancels() {
        let registry = JobRegistry::default();
        let result = map_task_create(
            TaskCreateRequest { description: "x".to_string(), instructions: None, cwd: None },
            &test_thread(),
            None,
            &registry,
        ).await;
        let stopped = map_task_stop(&result.job_id, None, &registry).await.unwrap();
        assert_eq!(stopped.status, JobStatus::Cancelled);
    }
}
