use serde::{Deserialize, Serialize};

use crate::app_server::session::DelegationPolicy;
use crate::app_server::thread::BridgeThread;
use crate::app_server::UserInput;
use crate::jobs::model::{unix_timestamp_now, JobKind, JobRecord, JobStatus};
use crate::jobs::registry::JobRegistry;
use crate::jobs::{ExecutorRequest, JobExecutor};
use crate::mapping::tools::ToolWarning;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSpawnRequest {
    pub task: String,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSpawnResult {
    pub job_id: String,
    pub allowed: bool,
    pub reason: Option<String>,
    pub warnings: Vec<ToolWarning>,
}

/// Check DelegationPolicy and conditionally spawn subagent (P3-010).
pub async fn map_agent_spawn(
    request: AgentSpawnRequest,
    thread: &BridgeThread,
    policy: &DelegationPolicy,
    executor: Option<&JobExecutor>,
    registry: &JobRegistry,
) -> AgentSpawnResult {
    match policy {
        DelegationPolicy::Never => AgentSpawnResult {
            job_id: String::new(),
            allowed: false,
            reason: Some("DelegationPolicy::Never — subagent spawn rejected".to_string()),
            warnings: Vec::new(),
        },
        DelegationPolicy::ExplicitOnly
        | DelegationPolicy::Heuristic
        | DelegationPolicy::ForceForSurface(_) => {
            if let Some(executor) = executor {
                if let Ok(start) = executor
                    .start_job(ExecutorRequest {
                        origin_surface_id: "tool.agent".to_string(),
                        kind: JobKind::Subagent,
                        cwd: request.cwd.clone().unwrap_or_else(|| thread.cwd.clone()),
                        model: "gpt-5.4".to_string(),
                        developer_instructions: None,
                        input: vec![UserInput::Text {
                            text: request.task.clone(),
                        }],
                        existing_thread_id: None,
                        client_session_id: None,
                        account_id: None,
                        account_auth_path: None,
                    })
                    .await
                {
                    return AgentSpawnResult {
                        job_id: start.job_id,
                        allowed: true,
                        reason: None,
                        warnings: Vec::new(),
                    };
                }
            }

            let job_id = format!("agent-{}", simple_id());
            let job = JobRecord {
                job_id: job_id.clone(),
                origin_surface_id: "tool.agent".to_string(),
                kind: JobKind::Subagent,
                status: JobStatus::Queued,
                scheduler_mode: None,
                codex_thread_id: Some(thread.thread_id.clone()),
                codex_turn_id: None,
                codex_agent_ids: Vec::new(),
                worktree_path: None,
                account_id: None,
                account_auth_path: None,
                created_at: unix_timestamp_now(),
                finished_at: None,
                result_summary: Some(request.task.clone()),
                warnings: Vec::new(),
                error_message: None,
            };
            registry.insert(job).await;
            AgentSpawnResult {
                job_id,
                allowed: true,
                reason: None,
                warnings: Vec::new(),
            }
        }
    }
}

/// Map SendMessage to inter-agent communication (P3-011).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageRequest {
    pub agent_id: String,
    pub message: String,
}

pub async fn map_send_message(
    request: SendMessageRequest,
    executor: Option<&JobExecutor>,
    registry: &JobRegistry,
) -> Result<(), String> {
    if let Some(job) = registry.get(&request.agent_id).await {
        if job.kind == JobKind::Subagent && job.status == JobStatus::Running {
            if let Some(executor) = executor {
                executor
                    .send_input(&request.agent_id, request.message)
                    .await
                    .map_err(|err| err.to_string())
            } else {
                Ok(())
            }
        } else {
            Err(format!("Agent {} is not running", request.agent_id))
        }
    } else {
        Err(format!("Agent {} not found", request.agent_id))
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
    use crate::app_server::thread::BridgeThread;
    use crate::mapping::approvals::{ApprovalPolicy, SandboxConfig};

    fn test_thread() -> BridgeThread {
        BridgeThread {
            thread_id: "t1".to_string(),
            bridge_session_id: "s1".to_string(),
            cwd: "/tmp".to_string(),
            project_root: None,
            approval_policy: ApprovalPolicy::OnRequest,
            sandbox_config: SandboxConfig::WorkspaceWrite,
            created_at_unix: 0,
            turn_count: 0,
        }
    }

    // P3-T03: Agent with DelegationPolicy.ExplicitOnly
    #[tokio::test]
    async fn agent_explicit_only_allowed() {
        let registry = JobRegistry::default();
        let result = map_agent_spawn(
            AgentSpawnRequest {
                task: "review code".to_string(),
                cwd: None,
            },
            &test_thread(),
            &DelegationPolicy::ExplicitOnly,
            None,
            &registry,
        )
        .await;
        assert!(result.allowed);
        assert!(registry.get(&result.job_id).await.is_some());
    }

    // P3-T04: Agent with DelegationPolicy.Never → rejected
    #[tokio::test]
    async fn agent_never_rejected() {
        let registry = JobRegistry::default();
        let result = map_agent_spawn(
            AgentSpawnRequest {
                task: "review code".to_string(),
                cwd: None,
            },
            &test_thread(),
            &DelegationPolicy::Never,
            None,
            &registry,
        )
        .await;
        assert!(!result.allowed);
        assert!(result.reason.is_some());
    }

    // P3-T07: child subagent approval isolated from parent
    // Subagent gets its own JobRecord; parent thread is not modified.
    #[tokio::test]
    async fn child_subagent_approval_isolated_from_parent() {
        let registry = JobRegistry::default();
        let parent_thread = test_thread();

        // Spawn child subagent
        let child = map_agent_spawn(
            AgentSpawnRequest {
                task: "fix bug".to_string(),
                cwd: None,
            },
            &parent_thread,
            &DelegationPolicy::ExplicitOnly,
            None,
            &registry,
        )
        .await;
        assert!(child.allowed);

        // Child job exists independently
        let child_job = registry.get(&child.job_id).await.unwrap();
        assert_eq!(child_job.kind, JobKind::Subagent);
        assert_eq!(child_job.codex_thread_id.as_deref(), Some("t1"));

        // Parent thread state is not mutated by child spawn
        assert_eq!(parent_thread.turn_count, 0);
        assert_eq!(parent_thread.approval_policy, ApprovalPolicy::OnRequest);

        // Multiple children don't interfere with each other
        let child2 = map_agent_spawn(
            AgentSpawnRequest {
                task: "review code".to_string(),
                cwd: None,
            },
            &parent_thread,
            &DelegationPolicy::ExplicitOnly,
            None,
            &registry,
        )
        .await;
        assert!(child2.allowed);
        assert_ne!(child.job_id, child2.job_id);
    }
}
