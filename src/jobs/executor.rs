use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::{broadcast, mpsc, RwLock};
use uuid::Uuid;

use crate::accounts::AccountPool;
use crate::app_server::{
    ApiStability, AppServerClient, AppServerEvent, AppServerEventKind, BridgeSession, BridgeThread,
    DelegationPolicy, JsonRpcNotification, JsonRpcRequest, ThreadStartRequest, ThreadStartResult,
    TransportKind, TurnStartRequest, UserInput,
};
use crate::mapping::approvals::{ApprovalPolicy, ApprovalResponse, SandboxConfig};
use crate::mapping::interaction::{classify_interaction, InteractionClassification};
use crate::model_profiles::resolve_model_profile;
use crate::state::StateStore;
use crate::surfaces::OperationMode;

use super::{unix_timestamp_now, JobKind, JobRecord, JobRegistry, JobStatus};
use super::{ThreadLease, ThreadPool, ThreadReuseConfig};

#[derive(Debug, Clone)]
pub struct JobExecutor {
    client: Arc<AppServerClient>,
    jobs: JobRegistry,
    sessions: StateStore,
    subscribers: Arc<RwLock<HashMap<String, Vec<mpsc::UnboundedSender<AppServerEvent>>>>>,
    event_history: Arc<RwLock<HashMap<String, Vec<AppServerEvent>>>>,
    thread_pool: ThreadPool,
    reuse_config: ThreadReuseConfig,
    operation_mode: OperationMode,
    api_stability: ApiStability,
    delegation_policy: DelegationPolicy,
    pool: Option<Arc<AccountPool>>,
}

#[derive(Debug, Clone)]
pub struct ExecutorRequest {
    pub origin_surface_id: String,
    pub kind: JobKind,
    pub cwd: String,
    pub model: String,
    pub developer_instructions: Option<String>,
    pub input: Vec<UserInput>,
    pub existing_thread_id: Option<String>,
    pub client_session_id: Option<String>,
    pub account_id: Option<String>,
    pub account_auth_path: Option<String>,
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
    thread_pool: ThreadPool,
    pool: Option<Arc<AccountPool>>,
}

#[derive(Debug, Clone)]
struct SessionBinding {
    job_id: String,
    client_session_id: Option<String>,
    account_id: Option<String>,
    account_auth_path: Option<String>,
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
            None,
        )
    }

    pub fn with_runtime(
        client: AppServerClient,
        jobs: JobRegistry,
        sessions: StateStore,
        operation_mode: OperationMode,
        api_stability: ApiStability,
        delegation_policy: DelegationPolicy,
        pool: Option<Arc<AccountPool>>,
    ) -> Self {
        Self {
            client: Arc::new(client),
            jobs,
            sessions,
            subscribers: Arc::new(RwLock::new(HashMap::new())),
            event_history: Arc::new(RwLock::new(HashMap::new())),
            thread_pool: ThreadPool::default(),
            reuse_config: ThreadReuseConfig::from_env(),
            operation_mode,
            api_stability,
            delegation_policy,
            pool,
        }
    }

    async fn admit_thread(
        &self,
        request: &ExecutorRequest,
        backend_model: &str,
    ) -> Result<ThreadStartResult> {
        let thread = self
            .client
            .thread_start(ThreadStartRequest {
                cwd: Some(request.cwd.clone()),
                approval_policy: Some(ApprovalPolicy::OnRequest),
                sandbox: Some(SandboxConfig::WorkspaceWrite),
                model: Some(backend_model.to_string()),
                model_provider: None,
                developer_instructions: request.developer_instructions.clone(),
                base_instructions: None,
                ephemeral: Some(true),
            })
            .await?;

        self.thread_pool
            .register_admitted(&thread.thread_id, std::time::Instant::now())
            .await;
        Ok(thread)
    }

    async fn start_turn_on_thread(
        &self,
        thread_id: &str,
        request: &ExecutorRequest,
        backend_model: &str,
        effort: Option<String>,
    ) -> Result<(crate::app_server::TurnStartResult, Option<ThreadLease>)> {
        let lease = if request.existing_thread_id.is_some() {
            self.thread_pool
                .checkout(thread_id, std::time::Instant::now(), &self.reuse_config)
                .await
        } else {
            None
        };

        let turn = self
            .client
            .turn_start(TurnStartRequest {
                thread_id: thread_id.to_string(),
                input: request.input.clone(),
                approval_policy: None,
                cwd: Some(request.cwd.clone()),
                model: Some(backend_model.to_string()),
                sandbox_policy: None,
                effort: effort.clone(),
                summary: effort.map(|_| "auto".to_string()),
            })
            .await;

        match turn {
            Ok(turn) => Ok((turn, lease)),
            Err(err) => {
                if request.existing_thread_id.is_some() {
                    self.thread_pool.invalidate(thread_id).await;
                }
                Err(err)
            }
        }
    }

    pub async fn start_job(&self, request: ExecutorRequest) -> Result<ExecutorStartResult> {
        let resolved_model = resolve_model_profile(&request.model);
        let effort = resolved_model.effort.clone();
        let (thread_id, turn_id, lease, admitted_thread) =
            if let Some(existing_thread_id) = request.existing_thread_id.clone() {
                let (turn, lease) = self
                    .start_turn_on_thread(
                        &existing_thread_id,
                        &request,
                        &resolved_model.backend_model,
                        effort,
                    )
                    .await?;
                (existing_thread_id, turn.turn_id, lease, None)
            } else {
                let thread = self
                    .admit_thread(&request, &resolved_model.backend_model)
                    .await?;
                let (turn, lease) = self
                    .start_turn_on_thread(
                        &thread.thread_id,
                        &request,
                        &resolved_model.backend_model,
                        effort,
                    )
                    .await?;
                (thread.thread_id.clone(), turn.turn_id, lease, Some(thread))
            };

        let job_id = format!("job-{}", Uuid::new_v4());
        let session_binding = SessionBinding {
            job_id: job_id.clone(),
            client_session_id: request.client_session_id.clone(),
            account_id: request.account_id.clone(),
            account_auth_path: request.account_auth_path.clone(),
        };
        let account_id = request.account_id.clone();
        let account_auth_path = request.account_auth_path.clone();
        let job = JobRecord {
            job_id: job_id.clone(),
            origin_surface_id: request.origin_surface_id,
            kind: request.kind,
            status: JobStatus::Running,
            scheduler_mode: None,
            codex_thread_id: Some(thread_id.clone()),
            codex_turn_id: Some(turn_id.clone()),
            codex_agent_ids: Vec::new(),
            worktree_path: None,
            account_id,
            account_auth_path,
            created_at: unix_timestamp_now(),
            finished_at: None,
            result_summary: None,
            warnings: Vec::new(),
            error_message: None,
        };
        self.jobs.insert(job).await;
        self.subscribers
            .write()
            .await
            .insert(job_id.clone(), Vec::new());
        self.event_history
            .write()
            .await
            .insert(job_id.clone(), Vec::new());
        if let Some(thread) = admitted_thread.as_ref() {
            remember_session(
                &self.sessions,
                thread,
                self.operation_mode,
                self.api_stability,
                self.delegation_policy.clone(),
                &session_binding,
            )
            .await;
        } else {
            remember_existing_thread(
                &self.sessions,
                &thread_id,
                &request.cwd,
                self.operation_mode,
                self.api_stability,
                self.delegation_policy.clone(),
                &session_binding,
            )
            .await;
        }

        let jobs = self.jobs.clone();
        let job_id_for_task = job_id.clone();
        let driver_context = JobDriverContext {
            notifications: self.client.subscribe_notifications(),
            server_requests: self.client.subscribe_server_requests(),
            jobs: jobs.clone(),
            sessions: self.sessions.clone(),
            subscribers: self.subscribers.clone(),
            event_history: self.event_history.clone(),
            client: self.client.clone(),
            thread_pool: self.thread_pool.clone(),
            pool: self.pool.clone(),
        };

        spawn_job_driver(
            jobs,
            job_id_for_task,
            thread_id.clone(),
            turn_id.clone(),
            lease,
            driver_context,
        );

        Ok(ExecutorStartResult {
            job_id,
            thread_id,
            turn_id,
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
        let mut rx = self
            .subscribe(job_id)
            .await
            .ok_or(JobCollectionError::NotFound)?;
        let mut events = Vec::new();

        match tokio::time::timeout(timeout, async {
            loop {
                let Some(event) = rx.recv().await else {
                    break;
                };
                let done = matches!(
                    event.kind,
                    AppServerEventKind::TurnCompleted | AppServerEventKind::Error
                );
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
        let cwd = self
            .sessions
            .get_session(&thread_id)
            .await
            .map(|session| session.thread.cwd)
            .unwrap_or_else(|| ".".to_string());
        let request = build_follow_up_request(&job, cwd, text)?;
        let (turn, lease) = self
            .start_turn_on_thread(&thread_id, &request, "gpt-5.4", None)
            .await?;

        job.codex_turn_id = Some(turn.turn_id.clone());
        job.status = JobStatus::Running;
        job.error_message = None;
        self.jobs.insert(job).await;
        let driver_context = JobDriverContext {
            notifications: self.client.subscribe_notifications(),
            server_requests: self.client.subscribe_server_requests(),
            jobs: self.jobs.clone(),
            sessions: self.sessions.clone(),
            subscribers: self.subscribers.clone(),
            event_history: self.event_history.clone(),
            client: self.client.clone(),
            thread_pool: self.thread_pool.clone(),
            pool: self.pool.clone(),
        };
        spawn_job_driver(
            self.jobs.clone(),
            job_id.to_string(),
            thread_id,
            turn.turn_id,
            lease,
            driver_context,
        );
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

fn spawn_job_driver(
    jobs: JobRegistry,
    job_id: String,
    thread_id: String,
    turn_id: String,
    lease: Option<ThreadLease>,
    driver_context: JobDriverContext,
) {
    tokio::spawn(async move {
        if let Err(err) = drive_job_events(
            job_id.clone(),
            thread_id.clone(),
            turn_id.clone(),
            lease,
            driver_context,
        )
        .await
        {
            log::error!(
                "job executor failed for job_id={} thread_id={} turn_id={}: {}",
                job_id,
                thread_id,
                turn_id,
                err
            );
            if let Some(mut job) = jobs.get(&job_id).await {
                if !matches!(
                    job.status,
                    JobStatus::Completed | JobStatus::Cancelled | JobStatus::Failed
                ) {
                    job.status = JobStatus::Failed;
                    job.error_message = Some(err.to_string());
                    job.finished_at = Some(unix_timestamp_now());
                    jobs.insert(job).await;
                }
            }
        }
    });
}

pub async fn apply_notification_event(
    job_id: String,
    expected_thread_id: String,
    expected_turn_id: String,
    event: AppServerEvent,
    jobs: JobRegistry,
    text: &mut String,
    had_output: &mut bool,
) -> Result<bool> {
    if event.thread_id.as_deref() != Some(expected_thread_id.as_str())
        || event.turn_id.as_deref() != Some(expected_turn_id.as_str())
    {
        return Ok(false);
    }

    if let Some(delta) = event.delta.as_deref() {
        text.push_str(delta);
        if !delta.is_empty() {
            *had_output = true;
        }
    }

    let mut job = jobs
        .get(&job_id)
        .await
        .ok_or_else(|| anyhow!("job not found: {job_id}"))?;

    match event.kind {
        AppServerEventKind::TerminalInteraction => {
            job.status = match classify_interaction(&event) {
                Some(InteractionClassification::Clarification(_)) => {
                    JobStatus::WaitingClarification
                }
                Some(InteractionClassification::Approval(_)) | None => JobStatus::WaitingApproval,
            };
            jobs.insert(job).await;
            Ok(false)
        }
        AppServerEventKind::ItemStarted | AppServerEventKind::ItemCompleted
            if event.item_type() == Some("function_call") =>
        {
            *had_output = true;
            if !matches!(
                job.status,
                JobStatus::Completed | JobStatus::Cancelled | JobStatus::Failed
            ) {
                job.status = JobStatus::Running;
            }
            jobs.insert(job).await;
            Ok(false)
        }
        AppServerEventKind::TurnCompleted => {
            if *had_output {
                job.status = JobStatus::Completed;
                job.result_summary = (!text.is_empty()).then_some(text.clone());
                job.error_message = None;
            } else {
                job.status = JobStatus::Failed;
                job.error_message = Some("Codex returned an empty response".to_string());
            }
            job.finished_at = Some(unix_timestamp_now());
            jobs.insert(job).await;
            Ok(true)
        }
        AppServerEventKind::Error => {
            job.status = JobStatus::Failed;
            job.error_message = event
                .error_message()
                .or_else(|| Some("app-server error event".to_string()));
            job.finished_at = Some(unix_timestamp_now());
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
    lease: Option<ThreadLease>,
    mut context: JobDriverContext,
) -> Result<()> {
    let mut text = String::new();
    let mut had_output = false;

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
                        event.clone(),
                        context.jobs.clone(),
                        &mut text,
                        &mut had_output,
                    ).await?;
                    if done {
                        let final_job = context.jobs.get(&job_id).await;
                        // Determine if this was the final turn for the job
                        let is_final_turn = final_job
                            .as_ref()
                            .map(|job| {
                                matches!(job.status, JobStatus::Completed
                                    | JobStatus::Cancelled
                                    | JobStatus::Failed)
                            })
                            .unwrap_or(false);
                        let success = final_job
                            .as_ref()
                            .map(|job| matches!(job.status, JobStatus::Completed))
                            .unwrap_or(false);
                        if success && !text.is_empty() {
                            remember_last_assistant_message(&context.sessions, &thread_id, &text)
                                .await;
                        }
                        // Only release the lease if this was the final turn of the job
                        if is_final_turn {
                            if let Some(lease) = lease.as_ref() {
                                context
                                    .thread_pool
                                    .release(lease, true, std::time::Instant::now())
                                    .await;
                            }
                        } else {
                            // Do not release yet: keep thread leased for potential continuation turns
                        }
                        // Update pool statistics if we know the final job state
                        if let (Some(pool), Some(job)) = (context.pool.as_ref(), final_job.as_ref()) {
                            if let Some(auth_path) = job.account_auth_path.as_ref() {
                                let auth_path = std::path::PathBuf::from(auth_path);
                                match job.status {
                                    JobStatus::Completed => pool.report_success(&auth_path).await,
                                    JobStatus::Failed | JobStatus::Cancelled => {
                                        let message = job
                                            .error_message
                                            .as_deref()
                                            .unwrap_or("app-server job failed");
                                        pool.report_error(&auth_path, message).await;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        context.subscribers.write().await.remove(&job_id);
                        forget_job_on_session(&context.sessions, &thread_id, &job_id).await;
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!("job driver lagged on shared app-server notification bus: skipped={skipped}");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    if let Some(lease) = lease.as_ref() {
                        context.thread_pool.invalidate(&lease.thread_id).await;
                    }
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
                    if let Some(lease) = lease.as_ref() {
                        context.thread_pool.invalidate(&lease.thread_id).await;
                    }
                    return Err(anyhow!("app-server server-request channel closed"));
                }
            }
        }
    }

    Ok(())
}

fn build_follow_up_request(job: &JobRecord, cwd: String, text: String) -> Result<ExecutorRequest> {
    let thread_id = job
        .codex_thread_id
        .clone()
        .ok_or_else(|| anyhow!("job missing thread id: {}", job.job_id))?;
    Ok(ExecutorRequest {
        origin_surface_id: job.origin_surface_id.clone(),
        kind: job.kind.clone(),
        cwd,
        model: "gpt-5.4".to_string(),
        developer_instructions: None,
        input: vec![UserInput::Text { text }],
        existing_thread_id: Some(thread_id),
        client_session_id: None,
        account_id: job.account_id.clone(),
        account_auth_path: job.account_auth_path.clone(),
    })
}

async fn remember_session(
    sessions: &StateStore,
    thread: &ThreadStartResult,
    operation_mode: OperationMode,
    api_stability: ApiStability,
    delegation_policy: DelegationPolicy,
    binding: &SessionBinding,
) {
    let previous = sessions.get_session(&thread.thread_id).await;
    let mut active_jobs = previous
        .as_ref()
        .map(|session| session.active_jobs.clone())
        .unwrap_or_default();
    if !active_jobs
        .iter()
        .any(|existing| existing == &binding.job_id)
    {
        active_jobs.push(binding.job_id.clone());
    }

    let session = BridgeSession {
        bridge_session_id: thread.thread_id.clone(),
        claude_session_id: binding.client_session_id.clone().or_else(|| {
            previous
                .as_ref()
                .and_then(|session| session.claude_session_id.clone())
        }),
        account_id: binding.account_id.clone().or_else(|| {
            previous
                .as_ref()
                .and_then(|session| session.account_id.clone())
        }),
        account_auth_path: binding.account_auth_path.clone().or_else(|| {
            previous
                .as_ref()
                .and_then(|session| session.account_auth_path.clone())
        }),
        last_assistant_message: previous
            .as_ref()
            .and_then(|session| session.last_assistant_message.clone()),
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

async fn remember_existing_thread(
    sessions: &StateStore,
    thread_id: &str,
    cwd: &str,
    operation_mode: OperationMode,
    api_stability: ApiStability,
    delegation_policy: DelegationPolicy,
    binding: &SessionBinding,
) {
    if let Some(mut session) = sessions.get_session(thread_id).await {
        if session.claude_session_id.is_none() {
            session.claude_session_id = binding.client_session_id.clone();
        }
        if session.account_id.is_none() {
            session.account_id = binding.account_id.clone();
        }
        if session.account_auth_path.is_none() {
            session.account_auth_path = binding.account_auth_path.clone();
        }
        if !session
            .active_jobs
            .iter()
            .any(|existing| existing == &binding.job_id)
        {
            session.active_jobs.push(binding.job_id.clone());
        }
        session.thread.turn_count += 1;
        sessions.insert_session(session).await;
        return;
    }

    sessions
        .insert_session(BridgeSession {
            bridge_session_id: thread_id.to_string(),
            claude_session_id: binding.client_session_id.clone(),
            account_id: binding.account_id.clone(),
            account_auth_path: binding.account_auth_path.clone(),
            last_assistant_message: None,
            thread: BridgeThread {
                thread_id: thread_id.to_string(),
                bridge_session_id: thread_id.to_string(),
                cwd: cwd.to_string(),
                project_root: None,
                approval_policy: ApprovalPolicy::OnRequest,
                sandbox_config: SandboxConfig::WorkspaceWrite,
                created_at_unix: chrono::Utc::now().timestamp(),
                turn_count: 1,
            },
            transport: TransportKind::Stdio,
            operation_mode,
            api_stability,
            delegation_policy,
            active_guidance_layers: Vec::new(),
            active_skills: Vec::new(),
            active_jobs: vec![binding.job_id.clone()],
            state_version: 1,
        })
        .await;
}

async fn forget_job_on_session(sessions: &StateStore, thread_id: &str, job_id: &str) {
    let Some(mut session) = sessions.get_session(thread_id).await else {
        return;
    };
    session.active_jobs.retain(|existing| existing != job_id);
    sessions.insert_session(session).await;
}

async fn remember_last_assistant_message(
    sessions: &StateStore,
    thread_id: &str,
    last_assistant_message: &str,
) {
    let Some(mut session) = sessions.get_session(thread_id).await else {
        return;
    };
    session.last_assistant_message = Some(last_assistant_message.to_string());
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
            account_id: None,
            account_auth_path: None,
            created_at: unix_timestamp_now(),
            finished_at: None,
            result_summary: None,
            warnings: Vec::new(),
            error_message: None,
        };
        jobs.insert(job).await;

        let mut text = String::new();
        let mut had_output = false;
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
            &mut had_output,
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
            &mut had_output,
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
            account_id: None,
            account_auth_path: None,
            created_at: unix_timestamp_now(),
            finished_at: None,
            result_summary: None,
            warnings: Vec::new(),
            error_message: None,
        })
        .await;

        let mut text = String::new();
        let mut had_output = false;
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
            &mut had_output,
        )
        .await
        .unwrap();

        let updated = jobs.get("job-1").await.unwrap();
        assert_eq!(updated.status, JobStatus::Queued);
        assert!(updated.result_summary.is_none());
    }

    #[tokio::test]
    async fn empty_turn_completion_marks_job_failed() {
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
            account_id: None,
            account_auth_path: None,
            created_at: unix_timestamp_now(),
            finished_at: None,
            result_summary: None,
            warnings: Vec::new(),
            error_message: None,
        })
        .await;

        let mut text = String::new();
        let mut had_output = false;
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
            &mut had_output,
        )
        .await
        .unwrap();

        let updated = jobs.get("job-1").await.unwrap();
        assert_eq!(updated.status, JobStatus::Failed);
        assert_eq!(
            updated.error_message.as_deref(),
            Some("Codex returned an empty response")
        );
        assert!(updated.finished_at.is_some());
    }

    #[test]
    fn follow_up_request_keeps_original_account_binding() {
        let job = JobRecord {
            job_id: "job-1".to_string(),
            origin_surface_id: "tool.task_create".to_string(),
            kind: JobKind::Task,
            status: JobStatus::Running,
            scheduler_mode: None,
            codex_thread_id: Some("thread-1".to_string()),
            codex_turn_id: Some("turn-1".to_string()),
            codex_agent_ids: Vec::new(),
            worktree_path: None,
            account_id: Some("account-a".to_string()),
            account_auth_path: Some("/tmp/account-a/auth.json".to_string()),
            created_at: unix_timestamp_now(),
            finished_at: None,
            result_summary: None,
            warnings: Vec::new(),
            error_message: None,
        };

        let request =
            build_follow_up_request(&job, "/workspace".to_string(), "continue".to_string())
                .expect("follow-up request");
        assert_eq!(request.existing_thread_id.as_deref(), Some("thread-1"));
        assert_eq!(request.account_id.as_deref(), Some("account-a"));
        assert_eq!(
            request.account_auth_path.as_deref(),
            Some("/tmp/account-a/auth.json")
        );
    }

    #[test]
    fn follow_up_request_requires_existing_thread_binding() {
        let job = JobRecord {
            job_id: "job-1".to_string(),
            origin_surface_id: "tool.task_create".to_string(),
            kind: JobKind::Task,
            status: JobStatus::Running,
            scheduler_mode: None,
            codex_thread_id: None,
            codex_turn_id: Some("turn-1".to_string()),
            codex_agent_ids: Vec::new(),
            worktree_path: None,
            account_id: Some("account-a".to_string()),
            account_auth_path: Some("/tmp/account-a/auth.json".to_string()),
            created_at: unix_timestamp_now(),
            finished_at: None,
            result_summary: None,
            warnings: Vec::new(),
            error_message: None,
        };

        let error = build_follow_up_request(&job, "/workspace".to_string(), "continue".to_string())
            .expect_err("missing thread should fail");
        assert!(error.to_string().contains("job missing thread id"));
    }
}
