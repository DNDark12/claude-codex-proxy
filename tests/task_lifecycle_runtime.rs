#[tokio::test]
#[ignore = "requires live codex app-server"]
async fn task_create_starts_running_job() {
    let client = claude_codex_proxy::app_server::AppServerClient::connect(
        claude_codex_proxy::app_server::AppServerConnectOptions::default(),
    )
    .await
    .unwrap();

    let jobs = claude_codex_proxy::jobs::JobRegistry::default();
    let sessions = claude_codex_proxy::state::StateStore::default();
    let executor =
        claude_codex_proxy::jobs::JobExecutor::new(client, jobs.clone(), sessions.clone());

    let result = executor
        .start_job(claude_codex_proxy::jobs::ExecutorRequest {
            origin_surface_id: "tool.task_create".to_string(),
            kind: claude_codex_proxy::jobs::JobKind::Task,
            cwd: std::env::current_dir().unwrap().display().to_string(),
            model: "gpt-5.4".to_string(),
            developer_instructions: None,
            input: vec![claude_codex_proxy::app_server::UserInput::Text {
                text: "Say hello and finish.".to_string(),
            }],
            existing_thread_id: None,
            client_session_id: None,
            account_id: None,
            account_auth_path: None,
        })
        .await
        .unwrap();

    let job = jobs.get(&result.job_id).await.unwrap();
    assert!(matches!(
        job.status,
        claude_codex_proxy::jobs::JobStatus::Running
            | claude_codex_proxy::jobs::JobStatus::Completed
    ));
    assert_eq!(
        job.codex_thread_id.as_deref(),
        Some(result.thread_id.as_str())
    );
}

#[tokio::test]
#[ignore = "requires live codex app-server"]
async fn task_stop_interrupts_running_turn() {
    let client = claude_codex_proxy::app_server::AppServerClient::connect(
        claude_codex_proxy::app_server::AppServerConnectOptions::default(),
    )
    .await
    .unwrap();
    let jobs = claude_codex_proxy::jobs::JobRegistry::default();
    let sessions = claude_codex_proxy::state::StateStore::default();
    let executor = claude_codex_proxy::jobs::JobExecutor::new(client, jobs.clone(), sessions);

    let start = executor
        .start_job(claude_codex_proxy::jobs::ExecutorRequest {
            origin_surface_id: "tool.task_create".to_string(),
            kind: claude_codex_proxy::jobs::JobKind::Task,
            cwd: std::env::current_dir().unwrap().display().to_string(),
            model: "gpt-5.4".to_string(),
            developer_instructions: None,
            input: vec![claude_codex_proxy::app_server::UserInput::Text {
                text: "Wait for interruption.".to_string(),
            }],
            existing_thread_id: None,
            client_session_id: None,
            account_id: None,
            account_auth_path: None,
        })
        .await
        .unwrap();

    executor.interrupt(&start.job_id).await.unwrap();
}

#[tokio::test]
#[ignore = "requires live codex app-server"]
async fn review_job_reaches_running_state() {
    let client = claude_codex_proxy::app_server::AppServerClient::connect(
        claude_codex_proxy::app_server::AppServerConnectOptions::default(),
    )
    .await
    .unwrap();
    let jobs = claude_codex_proxy::jobs::JobRegistry::default();
    let sessions = claude_codex_proxy::state::StateStore::default();
    let executor = claude_codex_proxy::jobs::JobExecutor::new(client, jobs.clone(), sessions);

    let start = executor
        .start_job(claude_codex_proxy::jobs::ExecutorRequest {
            origin_surface_id: "workflow.code_review".to_string(),
            kind: claude_codex_proxy::jobs::JobKind::Review,
            cwd: std::env::current_dir().unwrap().display().to_string(),
            model: "gpt-5.4".to_string(),
            developer_instructions: Some("Review this repository for bugs.".to_string()),
            input: vec![claude_codex_proxy::app_server::UserInput::Text {
                text: "Review the current workspace.".to_string(),
            }],
            existing_thread_id: None,
            client_session_id: None,
            account_id: None,
            account_auth_path: None,
        })
        .await
        .unwrap();

    let job = jobs.get(&start.job_id).await.unwrap();
    assert!(matches!(
        job.status,
        claude_codex_proxy::jobs::JobStatus::Running
            | claude_codex_proxy::jobs::JobStatus::Completed
            | claude_codex_proxy::jobs::JobStatus::WaitingApproval
    ));
}

#[tokio::test]
#[ignore = "requires live codex app-server"]
async fn concurrent_sessions_start_independent_jobs() {
    let client = claude_codex_proxy::app_server::AppServerClient::connect(
        claude_codex_proxy::app_server::AppServerConnectOptions::default(),
    )
    .await
    .unwrap();
    let jobs = claude_codex_proxy::jobs::JobRegistry::default();
    let sessions = claude_codex_proxy::state::StateStore::default();
    let executor = claude_codex_proxy::jobs::JobExecutor::new(client, jobs.clone(), sessions);

    let cwd = std::env::current_dir().unwrap().display().to_string();
    let first = executor.start_job(claude_codex_proxy::jobs::ExecutorRequest {
        origin_surface_id: "tool.task_create".to_string(),
        kind: claude_codex_proxy::jobs::JobKind::Task,
        cwd: cwd.clone(),
        model: "gpt-5.4".to_string(),
        developer_instructions: None,
        input: vec![claude_codex_proxy::app_server::UserInput::Text {
            text: "Say first.".to_string(),
        }],
        existing_thread_id: None,
        client_session_id: None,
        account_id: None,
        account_auth_path: None,
    });
    let second = executor.start_job(claude_codex_proxy::jobs::ExecutorRequest {
        origin_surface_id: "tool.task_create".to_string(),
        kind: claude_codex_proxy::jobs::JobKind::Task,
        cwd,
        model: "gpt-5.4".to_string(),
        developer_instructions: None,
        input: vec![claude_codex_proxy::app_server::UserInput::Text {
            text: "Say second.".to_string(),
        }],
        existing_thread_id: None,
        client_session_id: None,
        account_id: None,
        account_auth_path: None,
    });

    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap();
    let second = second.unwrap();
    assert_ne!(first.job_id, second.job_id);
    assert_ne!(first.thread_id, second.thread_id);

    let sessions = jobs.list().await;
    assert_eq!(sessions.len(), 2);
}

#[tokio::test]
#[ignore = "requires live codex app-server"]
async fn resume_uses_existing_thread_metadata() {
    let sessions = claude_codex_proxy::state::StateStore::default();
    sessions
        .insert_session(claude_codex_proxy::app_server::BridgeSession {
            bridge_session_id: "session-1".to_string(),
            claude_session_id: None,
            account_id: None,
            account_auth_path: None,
            last_assistant_message: None,
            thread: claude_codex_proxy::app_server::BridgeThread {
                thread_id: "thread-1".to_string(),
                bridge_session_id: "session-1".to_string(),
                cwd: std::env::current_dir().unwrap().display().to_string(),
                project_root: None,
                approval_policy: claude_codex_proxy::mapping::approvals::ApprovalPolicy::OnRequest,
                sandbox_config:
                    claude_codex_proxy::mapping::approvals::SandboxConfig::WorkspaceWrite,
                created_at_unix: 0,
                turn_count: 1,
            },
            transport: claude_codex_proxy::app_server::TransportKind::Stdio,
            operation_mode: claude_codex_proxy::surfaces::OperationMode::AutoHybrid,
            api_stability: claude_codex_proxy::app_server::ApiStability::Stable,
            delegation_policy: claude_codex_proxy::app_server::DelegationPolicy::ExplicitOnly,
            active_guidance_layers: Vec::new(),
            active_skills: Vec::new(),
            active_jobs: Vec::new(),
            state_version: 1,
        })
        .await;

    let result = claude_codex_proxy::mapping::workspace::map_resume(
        "thread-1",
        &claude_codex_proxy::jobs::JobRegistry::default(),
        &sessions,
    )
    .await
    .unwrap();
    assert_eq!(result.thread_id, "thread-1");
}
