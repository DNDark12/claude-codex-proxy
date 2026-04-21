use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::{broadcast, mpsc, RwLock};
use uuid::Uuid;

use crate::app_server::{
    ApiStability, AppServerClient, AppServerEvent, AppServerEventKind, BridgeSession,
    BridgeThread, DelegationPolicy, JsonRpcNotification, JsonRpcRequest, ThreadStartRequest,
    ThreadStartResult, TransportKind, TurnStartRequest, UserInput,
};
use crate::mapping::approvals::{ApprovalPolicy, ApprovalResponse, SandboxConfig};
use crate::mapping::interaction::{classify_interaction, InteractionClassification};
use crate::model_profiles::resolve_model_profile;
use crate::state::StateStore;
use crate::surfaces::OperationMode;

use super::{JobKind, JobRecord, JobRegistry, JobStatus};

#[derive(Debug, Clone)]
pub struct JobExecutor {
    client: Arc<AppServerClient>,
    jobs: JobRegistry,
    sessions: StateStore,
    subscribers: Arc<RwLock<HashMap<String, Vec<mpsc::UnboundedSender<AppServerEvent>>>>>,
    event_history: Arc<RwLock<HashMap<String, Vec<AppServerEvent>>>>,
    operation_mode: OperationMode,
    api_stability: ApiStability,
    delegation_policy: DelegationPolicy,
}

#[derive(Debug, Clone)]
pub struct ExecutorRequest {
    pub origin_surface_id: String,
    pub kind: JobKind,
    pub cwd: String,
    pub model: String,
    pub developer_instructions: Option<String>,
    pub input: Vec<UserInput>,
}

#[derive(Debug, Clone)]
pub struct ExecutorStartResult {
    pub job_id: String,
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobCollectionError {
    NotFound,
    Timeout,
}

struct JobDriverContext {
    notifications: broadcast::Receiver<JsonRpcNotification>,
    server_requests: broadcast::Receiver<JsonRpcRequest>,
    jobs: JobRegistry,
    sessions: StateStore,
    subscribers: Arc<RwLock<HashMap<String, Vec<mpsc::UnboundedSender<AppServerEvent>>>>>,
    event_history: Arc<RwLock<HashMap<String, Vec<AppServerEvent>>>>,
    client: Arc<AppServerClient>,
}

impl JobExecutor {
    pub fn new(client: AppServerClient, jobs: JobRegistry, sessions: StateStore) -> Self {
        Self::with_runtime(
            client,
            jobs,
            sessions,
            OperationMode::AutoHybrid,
            ApiStability::Stable,
            DelegationPolicy::ExplicitOnly,
        )
    }

    pub fn with_runtime(
        client: AppServerClient,
        jobs: JobRegistry,
        sessions: StateStore,
        operation_mode: OperationMode,
        api_stability: ApiStability,
        delegation_policy: DelegationPolicy,
    ) -> Self {
        Self {
            client: Arc::new(client),
            jobs,
            sessions,
            subscribers: Arc::new(RwLock::new(HashMap::new())),
            event_history: Arc::new(RwLock::new(HashMap::new())),
            operation_mode,
            api_stability,
            delegation_policy,
        }
    }

    pub async fn start_job(&self, request: ExecutorRequest) -> Result<ExecutorStartResult> {
        let resolved_model = resolve_model_profile(&request.model);
        let thread = self
            .client
            .thread_start(ThreadStartRequest {
                cwd: Some(request.cwd.clone()),
                approval_policy: Some(ApprovalPolicy::OnRequest),
                sandbox: Some(SandboxConfig::WorkspaceWrite),
                model: Some(resolved_model.backend_model.clone()),
                model_provider: None,
                developer_instructions: request.developer_instructions,
                base_instructions: None,
                ephemeral: Some(true),
            })
            .await?;

        let turn = self
            .client
            .turn_start(TurnStartRequest {
                thread_id: thread.thread_id.clone(),
                input: request.input,
                approval_policy: None,
                cwd: Some(thread.cwd.clone()),
                model: Some(resolved_model.backend_model),
                sandbox_policy: None,
                effort: resolved_model.effort.clone(),
                summary: resolved_model.effort.map(|_| "auto".to_string()),
            })
            .await?;

        let job_id = format!("job-{}", Uuid::new_v4());
        let job = JobRecord {
            job_id: job_id.clone(),
            origin_surface_id: request.origin_surface_id,
            kind: request.kind,
            status: JobStatus::Running,
            scheduler_mode: None,
            codex_thread_id: Some(thread.thread_id.clone()),
            codex_turn_id: Some(turn.turn_id.clone()),
            codex_agent_ids: Vec::new(),
            worktree_path: None,
            result_summary: None,
            warnings: Vec::new(),
            error_message: None,
        };
        self.jobs.insert(job).await;
        self.subscribers.write().await.insert(job_id.clone(), Vec::new());
        self.event_history
            .write()
            .await
            .insert(job_id.clone(), Vec::new());
        remember_session(
            &self.sessions,
            &thread,
            self.operation_mode,
            self.api_stability,
            self.delegation_policy.clone(),
            &job_id,
        )
        .await;

        let jobs = self.jobs.clone();
        let job_id_for_task = job_id.clone();
        let thread_id = thread.thread_id.clone();
        let turn_id = turn.turn_id.clone();
        let driver_context = JobDriverContext {
            notifications: self.client.subscribe_notifications(),
            server_requests: self.client.subscribe_server_requests(),
            jobs: jobs.clone(),
            sessions: self.sessions.clone(),
            subscribers: self.subscribers.clone(),
            event_history: self.event_history.clone(),
            client: self.client.clone(),
        };

        tokio::spawn(async move {
            if let Err(err) = drive_job_events(
                job_id_for_task.clone(),
                thread_id.clone(),
                turn_id.clone(),
                driver_context,
            )
            .await
            {
                log::error!(
                    "job executor failed for job_id={} thread_id={} turn_id={}: {}",
                    job_id_for_task,
                    thread_id,
                    turn_id,
                    err
                );
                if let Some(mut job) = jobs.get(&job_id_for_task).await {
                    if !matches!(
                        job.status,
                        JobStatus::Completed | JobStatus::Cancelled | JobStatus::Failed
                    ) {
                        job.status = JobStatus::Failed;
                        job.error_message = Some(err.to_string());
                        jobs.insert(job).await;
                    }
                }
            }
        });

        Ok(ExecutorStartResult {
            job_id,
            thread_id: thread.thread_id,
            turn_id: turn.turn_id,
        })
    }

    pub async fn subscribe(&self, job_id: &str) -> Option<mpsc::UnboundedReceiver<AppServerEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let history = self.event_history.read().await.get(job_id)?.clone();
        for event in history {
            let _ = tx.send(event);
        }
        let mut guard = self.subscribers.write().await;
        let entries = guard.get_mut(job_id)?;
        entries.push(tx);
        Some(rx)
    }

    pub async fn collect_until_complete(
        &self,
        job_id: &str,
        timeout: std::time::Duration,
    ) -> Result<Vec<AppServerEvent>, JobCollectionError> {
        let mut rx = self.subscribe(job_id).await.ok_or(JobCollectionError::NotFound)?;
        let mut events = Vec::new();

        match tokio::time::timeout(timeout, async {
            loop {
                let Some(event) = rx.recv().await else {
                    break;
                };
                let done = matches!(event.kind, AppServerEventKind::TurnCompleted);
                events.push(event);
                if done {
                    break;
                }
            }
        })
        .await
        {
            Ok(_) => Ok(events),
            Err(_) => Err(JobCollectionError::Timeout),
        }
    }

    pub async fn send_input(&self, job_id: &str, text: String) -> Result<()> {
        let mut job = self
            .jobs
            .get(job_id)
            .await
            .ok_or_else(|| anyhow!("job not found: {job_id}"))?;
        let thread_id = job
            .codex_thread_id
            .clone()
            .ok_or_else(|| anyhow!("job missing thread id: {job_id}"))?;

        let turn = self
            .client
            .turn_start(TurnStartRequest {
                thread_id,
                input: vec![UserInput::Text { text }],
                approval_policy: None,
                cwd: None,
                model: None,
                sandbox_policy: None,
                effort: None,
                summary: None,
            })
            .await?;

        job.codex_turn_id = Some(turn.turn_id);
        job.status = JobStatus::Running;
        job.error_message = None;
        self.jobs.insert(job).await;
        Ok(())
    }

    pub async fn interrupt(&self, job_id: &str) -> Result<()> {
        let job = self
            .jobs
            .get(job_id)
            .await
            .ok_or_else(|| anyhow!("job not found: {job_id}"))?;
        self.client
            .turn_interrupt(
                job.codex_thread_id
                    .as_deref()
                    .ok_or_else(|| anyhow!("job missing thread id: {job_id}"))?,
                job.codex_turn_id
                    .as_deref()
                    .ok_or_else(|| anyhow!("job missing turn id: {job_id}"))?,
            )
            .await?;
        Ok(())
    }
}

pub async fn apply_notification_event(
    job_id: String,
    expected_thread_id: String,
    expected_turn_id: String,
    event: AppServerEvent,
    jobs: JobRegistry,
    text: &mut String,
) -> Result<bool> {
    if event.thread_id.as_deref() != Some(expected_thread_id.as_str())
        || event.turn_id.as_deref() != Some(expected_turn_id.as_str())
    {
        return Ok(false);
    }

    if let Some(delta) = event.delta.as_deref() {
        text.push_str(delta);
    }

    let mut job = jobs
        .get(&job_id)
        .await
        .ok_or_else(|| anyhow!("job not found: {job_id}"))?;

    match event.kind {
        AppServerEventKind::TerminalInteraction => {
            job.status = match classify_interaction(&event) {
                Some(InteractionClassification::Clarification(_)) => JobStatus::WaitingClarification,
                Some(InteractionClassification::Approval(_)) | None => JobStatus::WaitingApproval,
            };
            jobs.insert(job).await;
            Ok(false)
        }
        AppServerEventKind::TurnCompleted => {
            job.status = JobStatus::Completed;
            job.result_summary = (!text.is_empty()).then_some(text.clone());
            job.error_message = None;
            jobs.insert(job).await;
            Ok(true)
        }
        AppServerEventKind::Error => {
            job.status = JobStatus::Failed;
            job.error_message = event
                .params
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| Some("app-server error event".to_string()));
            jobs.insert(job).await;
            Ok(true)
        }
        _ => {
            if !matches!(
                job.status,
                JobStatus::Completed | JobStatus::Cancelled | JobStatus::Failed
            ) {
                job.status = JobStatus::Running;
            }
            jobs.insert(job).await;
            Ok(false)
        }
    }
}

fn should_auto_approve_server_request(request: &JsonRpcRequest) -> bool {
    matches!(
        request.method.as_str(),
        "item/commandExecution/requestApproval"
            | "item/fileEdit/requestApproval"
            | "item/fileWrite/requestApproval"
            | "item/fileChange/requestApproval"
    )
}

async fn apply_server_request(
    job_id: &str,
    expected_thread_id: &str,
    expected_turn_id: &str,
    request: JsonRpcRequest,
    jobs: &JobRegistry,
    client: &Arc<AppServerClient>,
) -> Result<()> {
    let request_thread_id = request
        .params
        .get("threadId")
        .and_then(serde_json::Value::as_str);
    if request_thread_id != Some(expected_thread_id) {
        return Ok(());
    }

    if let Some(request_turn_id) = request
        .params
        .get("turnId")
        .and_then(serde_json::Value::as_str)
    {
        if request_turn_id != expected_turn_id {
            return Ok(());
        }
    }

    if should_auto_approve_server_request(&request) {
        client
            .respond_to_server_request(
                request.id,
                ApprovalResponse::Allow.to_server_value_for_method(&request.method),
            )
            .await?;
        return Ok(());
    }

    let mut job = jobs
        .get(job_id)
        .await
        .ok_or_else(|| anyhow!("job not found: {job_id}"))?;
    job.status = if request.method == "item/tool/requestUserInput" {
        JobStatus::WaitingClarification
    } else {
        JobStatus::WaitingApproval
    };
    jobs.insert(job).await;

    Ok(())
}

async fn drive_job_events(
    job_id: String,
    thread_id: String,
    turn_id: String,
    mut context: JobDriverContext,
) -> Result<()> {
    let mut text = String::new();

    loop {
        tokio::select! {
            notification = context.notifications.recv() => match notification {
                Ok(raw) => {
                    let event = AppServerEvent::from(raw);
                    if event.thread_id.as_deref() != Some(thread_id.as_str())
                        || event.turn_id.as_deref() != Some(turn_id.as_str())
                    {
                        continue;
                    }

                    {
                        let mut history = context.event_history.write().await;
                        if let Some(entries) = history.get_mut(&job_id) {
                            entries.push(event.clone());
                        }
                    }

                    let mut guard = context.subscribers.write().await;
                    if let Some(list) = guard.get_mut(&job_id) {
                        list.retain(|tx| tx.send(event.clone()).is_ok());
                    }
                    drop(guard);

                    let done = apply_notification_event(
                        job_id.clone(),
                        thread_id.clone(),
                        turn_id.clone(),
                        event,
                        context.jobs.clone(),
                        &mut text,
                    ).await?;
                    if done {
                        forget_job_on_session(&context.sessions, &thread_id, &job_id).await;
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!("job driver lagged on shared app-server notification bus: skipped={skipped}");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(anyhow!("app-server notification channel closed"));
                }
            },
            server_request = context.server_requests.recv() => match server_request {
                Ok(request) => {
                    apply_server_request(
                        &job_id,
                        &thread_id,
                        &turn_id,
                        request,
                        &context.jobs,
                        &context.client,
                    ).await?;
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!("job driver lagged on shared app-server request bus: skipped={skipped}");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(anyhow!("app-server server-request channel closed"));
                }
            }
        }
    }

    Ok(())
}

async fn remember_session(
    sessions: &StateStore,
    thread: &ThreadStartResult,
    operation_mode: OperationMode,
    api_stability: ApiStability,
    delegation_policy: DelegationPolicy,
    job_id: &str,
) {
    let previous = sessions.get_session(&thread.thread_id).await;
    let mut active_jobs = previous
        .as_ref()
        .map(|session| session.active_jobs.clone())
        .unwrap_or_default();
    if !active_jobs.iter().any(|existing| existing == job_id) {
        active_jobs.push(job_id.to_string());
    }

    let session = BridgeSession {
        bridge_session_id: thread.thread_id.clone(),
        claude_session_id: None,
        thread: BridgeThread {
            thread_id: thread.thread_id.clone(),
            bridge_session_id: thread.thread_id.clone(),
            cwd: thread.cwd.clone(),
            project_root: None,
            approval_policy: thread.approval_policy,
            sandbox_config: parse_sandbox_config(&thread.sandbox),
            created_at_unix: thread.created_at,
            turn_count: previous
                .as_ref()
                .map(|session| session.thread.turn_count + 1)
                .unwrap_or(1),
        },
        transport: TransportKind::Stdio,
        operation_mode,
        api_stability,
        delegation_policy,
        active_guidance_layers: previous
            .as_ref()
            .map(|session| session.active_guidance_layers.clone())
            .unwrap_or_default(),
        active_skills: previous
            .as_ref()
            .map(|session| session.active_skills.clone())
            .unwrap_or_default(),
        active_jobs,
        state_version: 1,
    };
    sessions.insert_session(session).await;
}

async fn forget_job_on_session(sessions: &StateStore, thread_id: &str, job_id: &str) {
    let Some(mut session) = sessions.get_session(thread_id).await else {
        return;
    };
    session.active_jobs.retain(|existing| existing != job_id);
    sessions.insert_session(session).await;
}

fn parse_sandbox_config(value: &serde_json::Value) -> SandboxConfig {
    if let Some(mode) = value.as_str() {
        return match mode {
            "read-only" | "readOnly" => SandboxConfig::ReadOnly,
            "workspace-write" | "workspaceWrite" => SandboxConfig::WorkspaceWrite,
            "danger-full-access" | "dangerFullAccess" => SandboxConfig::DangerFullAccess,
            _ => SandboxConfig::WorkspaceWrite,
        };
    }

    match value.get("type").and_then(|kind| kind.as_str()) {
        Some("readOnly") => SandboxConfig::ReadOnly,
        Some("workspaceWrite") => SandboxConfig::WorkspaceWrite,
        Some("dangerFullAccess") => SandboxConfig::DangerFullAccess,
        _ => SandboxConfig::WorkspaceWrite,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::JsonRpcNotification;
    use serde_json::json;

    #[tokio::test]
    async fn driver_marks_job_running_then_completed() {
        let jobs = JobRegistry::default();

        let job = JobRecord {
            job_id: "job-1".to_string(),
            origin_surface_id: "tool.task_create".to_string(),
            kind: JobKind::Task,
            status: JobStatus::Queued,
            scheduler_mode: None,
            codex_thread_id: Some("thread-1".to_string()),
            codex_turn_id: Some("turn-1".to_string()),
            codex_agent_ids: Vec::new(),
            worktree_path: None,
            result_summary: None,
            warnings: Vec::new(),
            error_message: None,
        };
        jobs.insert(job).await;

        let mut text = String::new();
        apply_notification_event(
            "job-1".to_string(),
            "thread-1".to_string(),
            "turn-1".to_string(),
            crate::app_server::AppServerEvent::from(JsonRpcNotification {
                jsonrpc: "2.0".to_string(),
                method: "item/agentMessage/delta".to_string(),
                params: json!({ "threadId": "thread-1", "turnId": "turn-1", "delta": "done" }),
            }),
            jobs.clone(),
            &mut text,
        )
        .await
        .unwrap();

        apply_notification_event(
            "job-1".to_string(),
            "thread-1".to_string(),
            "turn-1".to_string(),
            crate::app_server::AppServerEvent::from(JsonRpcNotification {
                jsonrpc: "2.0".to_string(),
                method: "turn/completed".to_string(),
                params: json!({ "threadId": "thread-1", "turnId": "turn-1" }),
            }),
            jobs.clone(),
            &mut text,
        )
        .await
        .unwrap();

        let updated = jobs.get("job-1").await.unwrap();
        assert_eq!(updated.status, JobStatus::Completed);
        assert_eq!(updated.result_summary.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn driver_ignores_events_for_other_turns() {
        let jobs = JobRegistry::default();
        jobs.insert(JobRecord {
            job_id: "job-1".to_string(),
            origin_surface_id: "tool.task_create".to_string(),
            kind: JobKind::Task,
            status: JobStatus::Queued,
            scheduler_mode: None,
            codex_thread_id: Some("thread-1".to_string()),
            codex_turn_id: Some("turn-1".to_string()),
            codex_agent_ids: Vec::new(),
            worktree_path: None,
            result_summary: None,
            warnings: Vec::new(),
            error_message: None,
        }).await;

        let mut text = String::new();
        apply_notification_event(
            "job-1".to_string(),
            "thread-1".to_string(),
            "turn-1".to_string(),
            crate::app_server::AppServerEvent::from(JsonRpcNotification {
                jsonrpc: "2.0".to_string(),
                method: "item/agentMessage/delta".to_string(),
                params: json!({ "threadId": "thread-2", "turnId": "turn-2", "delta": "other" }),
            }),
            jobs.clone(),
            &mut text,
        )
        .await
        .unwrap();

        let updated = jobs.get("job-1").await.unwrap();
        assert_eq!(updated.status, JobStatus::Queued);
        assert!(updated.result_summary.is_none());
    }
}
