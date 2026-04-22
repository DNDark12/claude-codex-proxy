use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use super::model::JobRecord;

#[derive(Debug, Clone)]
pub struct JobRegistry {
    jobs: Arc<RwLock<HashMap<String, JobRecord>>>,
    jsonl_path: Option<PathBuf>,
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self {
            jobs: Arc::new(RwLock::new(HashMap::new())),
            jsonl_path: None,
        }
    }
}

impl JobRegistry {
    pub async fn with_jsonl_path(path: PathBuf) -> Result<Self> {
        let jobs = crate::jobs::persistence::load_jobs(&path).await?;
        Ok(Self {
            jobs: Arc::new(RwLock::new(jobs)),
            jsonl_path: Some(path),
        })
    }

    async fn persist(&self) {
        if let Some(path) = &self.jsonl_path {
            let snapshot = self.jobs.read().await.clone();
            let _ = crate::jobs::persistence::save_jobs(path, &snapshot).await;
        }
    }

    pub async fn insert(&self, job: JobRecord) {
        self.jobs.write().await.insert(job.job_id.clone(), job);
        self.persist().await;
    }

    pub async fn get(&self, job_id: &str) -> Option<JobRecord> {
        self.jobs.read().await.get(job_id).cloned()
    }

    pub async fn list(&self) -> Vec<JobRecord> {
        let mut jobs: Vec<_> = self.jobs.read().await.values().cloned().collect();
        jobs.sort_by(|a, b| a.job_id.cmp(&b.job_id));
        jobs
    }

    pub async fn remove(&self, job_id: &str) -> Option<JobRecord> {
        let removed = self.jobs.write().await.remove(job_id);
        if removed.is_some() {
            self.persist().await;
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{JobKind, JobStatus};

    #[tokio::test]
    async fn registry_reloads_jobs_from_disk() {
        let path = std::env::temp_dir().join(format!("jobs-{}.jsonl", uuid::Uuid::new_v4()));
        let _ = tokio::fs::remove_file(&path).await;

        let registry = JobRegistry::with_jsonl_path(path.clone()).await.unwrap();
        registry
            .insert(JobRecord {
                job_id: "job-1".to_string(),
                origin_surface_id: "tool.task_create".to_string(),
                kind: JobKind::Task,
                status: JobStatus::Completed,
                scheduler_mode: None,
                codex_thread_id: Some("thread-1".to_string()),
                codex_turn_id: Some("turn-1".to_string()),
                codex_agent_ids: Vec::new(),
                worktree_path: None,
                account_id: None,
                account_auth_path: None,
                created_at: crate::jobs::unix_timestamp_now(),
                finished_at: None,
                result_summary: Some("done".to_string()),
                warnings: Vec::new(),
                error_message: None,
            })
            .await;

        let reloaded = JobRegistry::with_jsonl_path(path.clone()).await.unwrap();
        let job = reloaded.get("job-1").await.unwrap();
        assert_eq!(job.status, JobStatus::Completed);
        assert_eq!(job.result_summary.as_deref(), Some("done"));

        let _ = tokio::fs::remove_file(path).await;
    }
}
