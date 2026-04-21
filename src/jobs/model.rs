use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulingSurface {
    SessionCron,
    DurableRoutine,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerMode {
    SessionCron { session_id: String },
    DurableAutomation { automation_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Review,
    Rescue,
    Task,
    Schedule,
    Automation,
    Subagent,
    SessionCron,
    DurableAutomation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    WaitingApproval,
    WaitingClarification,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRecord {
    pub job_id: String,
    pub origin_surface_id: String,
    pub kind: JobKind,
    pub status: JobStatus,
    pub scheduler_mode: Option<SchedulerMode>,
    pub codex_thread_id: Option<String>,
    pub codex_turn_id: Option<String>,
    #[serde(default)]
    pub codex_agent_ids: Vec<String>,
    pub worktree_path: Option<String>,
    pub result_summary: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub error_message: Option<String>,
}
