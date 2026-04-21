use crate::jobs::model::{JobKind, JobStatus};
use crate::jobs::registry::JobRegistry;

/// Session-scoped scheduler: entries die when session ends (P5-006).
pub async fn cleanup_session_crons(session_id: &str, registry: &JobRegistry) {
    let jobs = registry.list().await;
    for mut job in jobs {
        if job.kind == JobKind::SessionCron
            && matches!(
                job.scheduler_mode.as_ref(),
                Some(crate::jobs::model::SchedulerMode::SessionCron { session_id: sid })
                    if sid == session_id
            )
        {
            job.status = JobStatus::Cancelled;
            registry.insert(job).await;
        }
    }
}
