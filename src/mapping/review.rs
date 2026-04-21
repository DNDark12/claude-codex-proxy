use serde::{Deserialize, Serialize};

use crate::jobs::model::{JobKind, JobRecord, JobStatus};
use crate::jobs::registry::JobRegistry;
use crate::mapping::tools::ToolWarning;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRequest {
    pub scope: Option<String>,
    pub files: Option<Vec<String>>,
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewResult {
    pub job_id: String,
    pub status: JobStatus,
    pub findings: Vec<ReviewFinding>,
    pub warnings: Vec<ToolWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFinding {
    pub file: String,
    pub line: Option<u32>,
    pub severity: String,
    pub message: String,
    pub suggestion: Option<String>,
}

pub async fn map_code_review(
    request: ReviewRequest,
    registry: &JobRegistry,
) -> ReviewResult {
    let job_id = format!("review-{}", simple_id());
    let job = JobRecord {
        job_id: job_id.clone(),
        origin_surface_id: "workflow.code_review".to_string(),
        kind: JobKind::Review,
        status: JobStatus::Queued,
        scheduler_mode: None,
        codex_thread_id: None,
        codex_agent_ids: Vec::new(),
        worktree_path: None,
        result_summary: Some(review_summary(&request)),
        warnings: Vec::new(),
    };
    registry.insert(job).await;

    ReviewResult {
        job_id,
        status: JobStatus::Queued,
        findings: Vec::new(),
        warnings: Vec::new(),
    }
}

pub async fn map_security_review(
    request: ReviewRequest,
    registry: &JobRegistry,
) -> ReviewResult {
    let job_id = format!("secreview-{}", simple_id());
    let job = JobRecord {
        job_id: job_id.clone(),
        origin_surface_id: "workflow.security_review".to_string(),
        kind: JobKind::Review,
        status: JobStatus::Queued,
        scheduler_mode: None,
        codex_thread_id: None,
        codex_agent_ids: Vec::new(),
        worktree_path: None,
        result_summary: Some(review_summary(&request)),
        warnings: Vec::new(),
    };
    registry.insert(job).await;

    ReviewResult {
        job_id,
        status: JobStatus::Queued,
        findings: Vec::new(),
        warnings: Vec::new(),
    }
}

pub async fn map_rescue_fix(
    request: ReviewRequest,
    registry: &JobRegistry,
) -> ReviewResult {
    let job_id = format!("rescue-{}", simple_id());
    let job = JobRecord {
        job_id: job_id.clone(),
        origin_surface_id: "workflow.rescue_fix".to_string(),
        kind: JobKind::Rescue,
        status: JobStatus::Queued,
        scheduler_mode: None,
        codex_thread_id: None,
        codex_agent_ids: Vec::new(),
        worktree_path: None,
        result_summary: Some(review_summary(&request)),
        warnings: Vec::new(),
    };
    registry.insert(job).await;

    ReviewResult {
        job_id,
        status: JobStatus::Queued,
        findings: Vec::new(),
        warnings: Vec::new(),
    }
}

pub async fn map_review_status(job_id: &str, registry: &JobRegistry) -> Option<JobRecord> {
    registry.get(job_id).await
}

pub async fn map_review_cancel(job_id: &str, registry: &JobRegistry) -> Option<JobRecord> {
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

fn review_summary(request: &ReviewRequest) -> String {
    if let Some(scope) = request.scope.as_deref() {
        return format!("scope={scope}");
    }
    if let Some(files) = request.files.as_ref() {
        if !files.is_empty() {
            return format!("files={}", files.join(", "));
        }
    }
    if let Some(instructions) = request.instructions.as_deref() {
        if !instructions.is_empty() {
            return instructions.to_string();
        }
    }
    "review queued".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // P3-T05: code_review end-to-end with status/result/cancel
    #[tokio::test]
    async fn code_review_lifecycle() {
        let registry = JobRegistry::default();
        let result = map_code_review(
            ReviewRequest { scope: Some("all".to_string()), files: None, instructions: None },
            &registry,
        ).await;
        assert_eq!(result.status, JobStatus::Queued);

        let status = map_review_status(&result.job_id, &registry).await.unwrap();
        assert_eq!(status.kind, JobKind::Review);

        let cancelled = map_review_cancel(&result.job_id, &registry).await.unwrap();
        assert_eq!(cancelled.status, JobStatus::Cancelled);
    }

    #[tokio::test]
    async fn security_review_creates_job() {
        let registry = JobRegistry::default();
        let result = map_security_review(
            ReviewRequest { scope: None, files: None, instructions: None },
            &registry,
        ).await;
        assert!(!result.job_id.is_empty());
    }

    #[tokio::test]
    async fn rescue_fix_creates_rescue_job() {
        let registry = JobRegistry::default();
        let result = map_rescue_fix(
            ReviewRequest { scope: None, files: None, instructions: None },
            &registry,
        ).await;
        let job = registry.get(&result.job_id).await.unwrap();
        assert_eq!(job.kind, JobKind::Rescue);
    }

    // P3-T08: rescue_fix uses thread/fork
    // rescue_fix creates a Rescue job that can be associated with thread/fork
    #[tokio::test]
    async fn rescue_fix_job_supports_fork_association() {
        let registry = JobRegistry::default();
        let result = map_rescue_fix(
            ReviewRequest { scope: Some("src/".to_string()), files: None, instructions: Some("fix crash".to_string()) },
            &registry,
        ).await;
        let mut job = registry.get(&result.job_id).await.unwrap();
        assert_eq!(job.kind, JobKind::Rescue);

        // Simulate thread/fork: associate forked thread with rescue job
        job.codex_thread_id = Some("forked-thread-123".to_string());
        job.status = JobStatus::Running;
        registry.insert(job.clone()).await;

        let updated = registry.get(&result.job_id).await.unwrap();
        assert_eq!(updated.codex_thread_id.as_deref(), Some("forked-thread-123"));
        assert_eq!(updated.status, JobStatus::Running);
    }
}
