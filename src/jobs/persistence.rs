use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tokio::fs;

use crate::jobs::JobRecord;

pub async fn load_jobs(path: &Path) -> Result<HashMap<String, JobRecord>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let content = fs::read_to_string(path).await?;
    let mut jobs = HashMap::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let job: JobRecord = serde_json::from_str(line)?;
        jobs.insert(job.job_id.clone(), job);
    }
    Ok(jobs)
}

pub async fn save_jobs(path: &PathBuf, jobs: &HashMap<String, JobRecord>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let mut entries = jobs.values().cloned().collect::<Vec<_>>();
    entries.sort_by(|left, right| left.job_id.cmp(&right.job_id));

    let body = entries
        .into_iter()
        .map(|job| serde_json::to_string(&job))
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");

    fs::write(path, format!("{body}\n")).await?;
    Ok(())
}
