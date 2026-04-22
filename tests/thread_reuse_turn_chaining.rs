#[tokio::test]
#[ignore = "requires live codex app-server and manual quota-state control"]
async fn existing_thread_accepts_follow_up_turn() {
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
                text: "Say ready.".to_string(),
            }],
            existing_thread_id: None,
            client_session_id: None,
            account_id: None,
            account_auth_path: None,
        })
        .await
        .unwrap();

    let thread_id = jobs
        .get(&start.job_id)
        .await
        .and_then(|job| job.codex_thread_id)
        .expect("thread id");

    let follow_up = executor
        .start_job(claude_codex_proxy::jobs::ExecutorRequest {
            origin_surface_id: "tool.task_update".to_string(),
            kind: claude_codex_proxy::jobs::JobKind::Task,
            cwd: std::env::current_dir().unwrap().display().to_string(),
            model: "gpt-5.4".to_string(),
            developer_instructions: None,
            input: vec![claude_codex_proxy::app_server::UserInput::Text {
                text: "Say follow-up.".to_string(),
            }],
            existing_thread_id: Some(thread_id),
            client_session_id: None,
            account_id: None,
            account_auth_path: None,
        })
        .await;

    assert!(follow_up.is_ok());
}
