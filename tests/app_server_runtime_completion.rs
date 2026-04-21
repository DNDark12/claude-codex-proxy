use futures::StreamExt;

#[tokio::test]
#[ignore = "requires live codex app-server and warp test harness"]
async fn streaming_request_uses_app_server_without_responses_fallback() {
    let surface_registry = claude_codex_proxy::surfaces::SurfaceRegistry::new();
    let compatibility_matrix =
        claude_codex_proxy::surfaces::CompatibilityMatrix::new(&surface_registry);
    let app_server = claude_codex_proxy::app_server::AppServerClient::connect(
        claude_codex_proxy::app_server::AppServerConnectOptions::default(),
    )
    .await
    .unwrap();
    let jobs = claude_codex_proxy::jobs::JobRegistry::default();
    let sessions = claude_codex_proxy::state::StateStore::default();
    let executor = claude_codex_proxy::jobs::JobExecutor::new(
        app_server.clone(),
        jobs.clone(),
        sessions.clone(),
    );

    let routes = claude_codex_proxy::routes::build_routes(claude_codex_proxy::routes::RouteBuildOptions {
        client: None,
        app_server: Some(app_server),
        executor: Some(executor),
        skill_registry: None,
        surface_registry,
        compatibility_matrix,
        job_registry: jobs,
        state_store: sessions,
        operation_mode: claude_codex_proxy::surfaces::OperationMode::AutoHybrid,
        api_stability: claude_codex_proxy::app_server::ApiStability::Stable,
        delegation_policy: claude_codex_proxy::app_server::DelegationPolicy::ExplicitOnly,
    });

    let (addr, server) = warp::serve(routes).bind_ephemeral(([127, 0, 0, 1], 0));
    tokio::spawn(server);

    let response = reqwest::Client::new()
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "gpt-5.4",
            "stream": true,
            "tools": [{ "name": "Read", "input_schema": { "type": "object", "properties": {} } }],
            "messages": [{ "role": "user", "content": "Say hello." }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let mut stream = response.bytes_stream();
    let first_chunk = tokio::time::timeout(std::time::Duration::from_secs(30), stream.next())
        .await
        .expect("timed out waiting for SSE chunk")
        .expect("stream ended before first SSE chunk")
        .expect("failed to read SSE chunk");
    let body = String::from_utf8(first_chunk.to_vec()).unwrap();
    assert!(body.contains("event:") || body.contains("data:"));
}

#[tokio::test]
#[ignore = "requires live codex app-server and warp test harness"]
async fn detached_job_remains_running_after_initial_request_scope() {
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
                text: "Sleep briefly and then answer done.".to_string(),
            }],
        })
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let job = jobs.get(&start.job_id).await.unwrap();
    assert!(matches!(
        job.status,
        claude_codex_proxy::jobs::JobStatus::Running
            | claude_codex_proxy::jobs::JobStatus::Completed
            | claude_codex_proxy::jobs::JobStatus::WaitingApproval
    ));
}
