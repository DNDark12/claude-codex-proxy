use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use super::model::JobRecord;

#[derive(Debug, Clone, Default)]
pub struct JobRegistry {
    jobs: Arc<RwLock<HashMap<String, JobRecord>>>,
}

impl JobRegistry {
    pub async fn insert(&self, job: JobRecord) {
        self.jobs.write().await.insert(job.job_id.clone(), job);
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
        self.jobs.write().await.remove(job_id)
    }
}
