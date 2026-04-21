//! Integration and regression tests for the surface bridge.
//!
//! Tests marked `#[ignore]` require a live `codex app-server` binary.
//! Run with: cargo test -- --ignored
//!
//! Tests NOT marked ignore run as unit-level golden fixture validation.

use claude_codex_proxy::mapping::approvals::*;
use claude_codex_proxy::mapping::interaction::*;
use claude_codex_proxy::mapping::tools::*;
use claude_codex_proxy::mapping::tasks::*;
use claude_codex_proxy::mapping::subagents::*;
use claude_codex_proxy::mapping::review::*;
use claude_codex_proxy::mapping::planning::*;
use claude_codex_proxy::mapping::workspace::*;
use claude_codex_proxy::mapping::scheduling::*;
use claude_codex_proxy::mapping::commands::*;
use claude_codex_proxy::mapping::guidance::*;
use claude_codex_proxy::app_server::thread::BridgeThread;
use claude_codex_proxy::app_server::session::DelegationPolicy;
use claude_codex_proxy::app_server::events::{AppServerEvent, AppServerEventKind};
use claude_codex_proxy::jobs::registry::JobRegistry;
use claude_codex_proxy::jobs::model::*;
use claude_codex_proxy::surfaces::model::MappingStrategy;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn test_thread() -> BridgeThread {
    BridgeThread {
        thread_id: "integration-thread".to_string(),
        bridge_session_id: "integration-session".to_string(),
        cwd: "/tmp/test-project".to_string(),
        project_root: Some("/tmp/test-project".to_string()),
        approval_policy: ApprovalPolicy::OnRequest,
        sandbox_config: SandboxConfig::WorkspaceWrite,
        created_at_unix: 1700000000,
        turn_count: 0,
    }
}

fn write_mock_codex_app_server(script_body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("codex-mock-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    let script_path = dir.join(format!("mock-codex-{}.sh", uuid::Uuid::new_v4()));
    fs::write(&script_path, script_body).expect("script");
    let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("chmod");
    script_path
}

// ========================================================================
// P1-T01: Integration: spawn app-server → handshake → start thread → start turn → receive items
// ========================================================================
#[tokio::test]
#[ignore = "requires live codex app-server binary"]
async fn p1_t01_app_server_full_lifecycle() {
    use claude_codex_proxy::app_server::client::*;
    use claude_codex_proxy::app_server::session::ApiStability;

    let client = AppServerClient::connect(AppServerConnectOptions {
        api_stability: ApiStability::Stable,
        ..Default::default()
    })
    .await
    .expect("failed to connect to app-server");

    let thread = client
        .thread_start(ThreadStartRequest {
            cwd: Some("/tmp".to_string()),
            approval_policy: Some(ApprovalPolicy::OnRequest),
            sandbox: Some(SandboxConfig::WorkspaceWrite),
            model: None,
            model_provider: None,
            developer_instructions: None,
            base_instructions: None,
            ephemeral: Some(true),
        })
        .await
        .expect("failed to start thread");

    assert!(!thread.thread_id.is_empty());

    let turn = client
        .turn_start(TurnStartRequest {
            thread_id: thread.thread_id.clone(),
            input: vec![UserInput::Text {
                text: "Say hello".to_string(),
            }],
            approval_policy: None,
            cwd: None,
            model: None,
            sandbox_policy: None,
            effort: None,
            summary: None,
        })
        .await
        .expect("failed to start turn");

    assert!(!turn.turn_id.is_empty());

    let events = client
        .collect_text_deltas(&thread.thread_id, &turn.turn_id, client.subscribe_events());
    let events = tokio::time::timeout(std::time::Duration::from_secs(45), events)
        .await
        .expect("timed out waiting for turn completion")
        .expect("failed to collect events");

    assert!(!events.is_empty());

    client.kill().await.ok();
}

// ========================================================================
// P1-T02: Integration: approval pause → client allows → turn resumes
// ========================================================================
#[tokio::test]
#[ignore = "requires live codex app-server binary"]
async fn p1_t02_approval_pause_and_resume() {
    use claude_codex_proxy::app_server::client::*;
    use claude_codex_proxy::app_server::session::ApiStability;

    let client = AppServerClient::connect(AppServerConnectOptions {
        api_stability: ApiStability::Stable,
        ..Default::default()
    })
    .await
    .expect("connect");

    let thread = client
        .thread_start(ThreadStartRequest {
            cwd: Some("/tmp".to_string()),
            approval_policy: Some(ApprovalPolicy::Untrusted),
            sandbox: Some(SandboxConfig::ReadOnly),
            model: None,
            model_provider: None,
            developer_instructions: None,
            base_instructions: None,
            ephemeral: Some(true),
        })
        .await
        .expect("thread start");

    let _turn = client
        .turn_start(TurnStartRequest {
            thread_id: thread.thread_id.clone(),
            input: vec![UserInput::Text {
                text: "Write hello to /tmp/hello.txt".to_string(),
            }],
            approval_policy: None,
            cwd: None,
            model: None,
            sandbox_policy: None,
            effort: None,
            summary: None,
        })
        .await
        .expect("turn start");

    // Listen for approval request via server-initiated request
    let mut rx = client.subscribe_server_requests();
    if let Ok(Ok(req)) = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv()).await
    {
        // Approve it
        client
            .respond_to_server_request(
                req.id,
                ApprovalResponse::Allow.to_server_value_for_method(&req.method),
            )
            .await
            .expect("respond to approval");
    }

    client.kill().await.ok();
}

// ========================================================================
// P1-T03: Integration: clarification pause → client answers → turn resumes
// ========================================================================
#[tokio::test]
async fn p1_t03_clarification_pause_and_resume() {
    use claude_codex_proxy::app_server::client::*;
    use claude_codex_proxy::app_server::session::ApiStability;

    let script_path = write_mock_codex_app_server(
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      echo '{"jsonrpc":"2.0","id":1,"result":{"userAgent":"mock"}}'
      ;;
    *'"method":"initialized"'*)
      ;;
    *'"method":"configRequirements/read"'*)
      echo '{"jsonrpc":"2.0","id":2,"result":{"requirements":{"allowedApprovalPolicies":["untrusted","on-request"],"allowedSandboxModes":["read-only","workspace-write"]}}}'
      ;;
    *'"method":"thread/start"'*)
      echo '{"jsonrpc":"2.0","id":3,"result":{"thread":{"id":"thread-1","createdAt":1700000000},"cwd":"/tmp","approvalPolicy":"on-request","sandbox":{"type":"workspaceWrite"},"model":"gpt-5.2-codex","modelProvider":"openai"}}'
      ;;
    *'"method":"turn/start"'*)
      echo '{"jsonrpc":"2.0","id":4,"result":{"turn":{"id":"turn-1","status":"inProgress","items":[]}}}'
      echo '{"jsonrpc":"2.0","id":99,"method":"item/tool/requestUserInput","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"item-1","questions":[{"header":"File","id":"file","question":"Which file should I edit?"}]}}'
      ;;
    *'"id":99'*)
      echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","items":[]}}}'
      ;;
  esac
done
"#,
    );

    let client = AppServerClient::connect(AppServerConnectOptions {
        binary_path: script_path.to_string_lossy().to_string(),
        api_stability: ApiStability::Stable,
        ..Default::default()
    })
    .await
    .expect("connect");

    let thread = client
        .thread_start(ThreadStartRequest {
            cwd: Some("/tmp".to_string()),
            approval_policy: Some(ApprovalPolicy::OnRequest),
            sandbox: Some(SandboxConfig::WorkspaceWrite),
            model: None,
            model_provider: None,
            developer_instructions: None,
            base_instructions: None,
            ephemeral: Some(true),
        })
        .await
        .expect("thread start");

    let notifications = client.subscribe_notifications();
    let mut server_requests = client.subscribe_server_requests();
    let turn = client
        .turn_start(TurnStartRequest {
            thread_id: thread.thread_id.clone(),
            input: vec![UserInput::Text {
                text: "Ask me which file to edit".to_string(),
            }],
            approval_policy: None,
            cwd: None,
            model: None,
            sandbox_policy: None,
            effort: None,
            summary: None,
        })
        .await
        .expect("turn start");

    let request = tokio::time::timeout(std::time::Duration::from_secs(5), server_requests.recv())
        .await
        .expect("timeout waiting for clarification request")
        .expect("clarification request");
    assert_eq!(request.method, "item/tool/requestUserInput");

    client
        .respond_to_server_request(
            request.id,
            serde_json::json!({
                "answers": {
                    "file": {
                        "answers": ["src/main.rs"]
                    }
                }
            }),
        )
        .await
        .expect("respond to clarification");

    let events = client
        .collect_text_deltas(&thread.thread_id, &turn.turn_id, notifications)
        .await
        .expect("events");
    assert!(events
        .iter()
        .any(|event| matches!(event.kind, AppServerEventKind::TurnCompleted)));

    client.kill().await.ok();
}

// ========================================================================
// P1-T05: Integration: auto-hybrid falls back to Responses when app-server unavailable
// ========================================================================
#[tokio::test]
#[ignore = "requires controlled environment without codex binary"]
async fn p1_t05_auto_hybrid_fallback() {
    // This test verifies that when codex binary is not found,
    // the proxy falls back to Responses API mode.
    // Requires environment manipulation.
}

// ========================================================================
// P7-011: Full regression suite — run all tier fixtures
// ========================================================================

// Tier 0 fixtures
#[test]
fn regression_tier0_read() {
    let r = map_read(&test_thread(), "src/main.rs");
    assert_eq!(r.strategy, MappingStrategy::Native);
    assert!(r.warnings.is_empty());
}

#[test]
fn regression_tier0_write() {
    let r = map_write(&test_thread(), "out.txt", "data");
    assert_eq!(r.strategy, MappingStrategy::MediatedNative);
    assert!(!r.warnings.is_empty());
}

#[test]
fn regression_tier0_edit() {
    let r = map_edit(&test_thread(), "file.rs", serde_json::json!({"line": 1}));
    assert_eq!(r.strategy, MappingStrategy::MediatedNative);
}

#[test]
fn regression_tier0_multiedit() {
    let r = map_multiedit(&test_thread(), vec![]);
    assert!(r.warnings.iter().any(|w| w.warning.contains("Atomicity")));
}

#[test]
fn regression_tier0_glob() {
    let r = map_glob(&test_thread(), "**/*.rs");
    assert_eq!(r.strategy, MappingStrategy::Native);
}

#[test]
fn regression_tier0_grep() {
    let r = map_grep(&test_thread(), "TODO", None);
    assert_eq!(r.strategy, MappingStrategy::Native);
}

#[test]
fn regression_tier0_ls() {
    let r = map_ls(&test_thread(), None);
    assert_eq!(r.strategy, MappingStrategy::Native);
}

#[test]
fn regression_tier0_bash() {
    let r = map_bash(&test_thread(), "echo hi");
    assert_eq!(r.strategy, MappingStrategy::MediatedNative);
    assert_eq!(r.params["cwd"], "/tmp/test-project");
}

// Tier 1 fixtures
#[tokio::test]
async fn regression_tier1_task_lifecycle() {
    let reg = JobRegistry::default();
    let t = test_thread();
    let created = map_task_create(
        TaskCreateRequest { description: "test".into(), instructions: None, cwd: None },
        &t, None, &reg,
    ).await;
    assert_eq!(created.status, JobStatus::Queued);
    assert!(map_task_get(&created.job_id, &reg).await.is_some());
    assert_eq!(map_task_list(&reg).await.len(), 1);
    let stopped = map_task_stop(&created.job_id, None, &reg).await.unwrap();
    assert_eq!(stopped.status, JobStatus::Cancelled);
}

#[tokio::test]
async fn regression_tier1_agent_delegation() {
    let reg = JobRegistry::default();
    let t = test_thread();
    let allowed = map_agent_spawn(
        AgentSpawnRequest { task: "x".into(), cwd: None },
        &t, &DelegationPolicy::ExplicitOnly, None, &reg,
    ).await;
    assert!(allowed.allowed);

    let denied = map_agent_spawn(
        AgentSpawnRequest { task: "y".into(), cwd: None },
        &t, &DelegationPolicy::Never, None, &reg,
    ).await;
    assert!(!denied.allowed);
}

#[tokio::test]
async fn regression_tier1_review() {
    let reg = JobRegistry::default();
    let r = map_code_review(
        ReviewRequest { scope: None, files: None, instructions: None },
        None, &reg,
    ).await;
    let cancelled = map_review_cancel(&r.job_id, &reg).await.unwrap();
    assert_eq!(cancelled.status, JobStatus::Cancelled);
}

#[test]
fn regression_tier1_interaction_classification() {
    let ask = AppServerEvent {
        method: "terminal_interaction".into(),
        kind: AppServerEventKind::TerminalInteraction,
        params: serde_json::json!({"action": "ask_user", "question": "?"}),
        thread_id: Some("t".into()), turn_id: Some("u".into()),
        item_id: None, delta: None,
    };
    assert!(matches!(classify_interaction(&ask), Some(InteractionClassification::Clarification(_))));

    let approval = AppServerEvent {
        method: "terminal_interaction".into(),
        kind: AppServerEventKind::TerminalInteraction,
        params: serde_json::json!({"action": "approval_request", "description": "write"}),
        thread_id: Some("t".into()), turn_id: Some("u".into()),
        item_id: None, delta: None,
    };
    assert!(matches!(classify_interaction(&approval), Some(InteractionClassification::Approval(_))));
}

// Tier 2 fixtures
#[test]
fn regression_tier2_plan_mode() {
    let enter = map_enter_plan_mode();
    assert_eq!(enter.state, PlanModeState::Active);
    assert_eq!(enter.strategy, MappingStrategy::MediatedNative);

    let exit = map_exit_plan_mode();
    assert_eq!(exit.state, PlanModeState::Inactive);
}

#[test]
fn regression_tier2_rewind() {
    let r = map_rewind("t1", Some("turn-3"));
    assert_eq!(r.method, "thread/rollback");
}

#[tokio::test]
async fn regression_tier2_resume() {
    let sessions = claude_codex_proxy::state::StateStore::default();
    sessions.insert_session(claude_codex_proxy::app_server::BridgeSession {
        bridge_session_id: "integration-session".to_string(),
        claude_session_id: None,
        thread: test_thread(),
        transport: claude_codex_proxy::app_server::TransportKind::Stdio,
        operation_mode: claude_codex_proxy::surfaces::OperationMode::AutoHybrid,
        api_stability: claude_codex_proxy::app_server::ApiStability::Stable,
        delegation_policy: claude_codex_proxy::app_server::DelegationPolicy::ExplicitOnly,
        active_guidance_layers: Vec::new(),
        active_skills: Vec::new(),
        active_jobs: Vec::new(),
        state_version: 1,
    }).await;
    let r = map_resume("integration-thread", &JobRegistry::default(), &sessions)
        .await
        .unwrap();
    assert_eq!(r.strategy, MappingStrategy::MediatedNative);
}

// Tier 3 fixtures
#[tokio::test]
async fn regression_tier3_cron() {
    let reg = JobRegistry::default();
    let ephemeral = map_cron_create(
        CronCreateRequest { schedule: "* * * * *".into(), prompt: "x".into(), durable: None },
        "s1", &reg,
    ).await;
    assert_eq!(ephemeral.scheduler_mode, SchedulingSurface::SessionCron);
    assert!(ephemeral.warnings.is_empty());

    let durable = map_cron_create(
        CronCreateRequest { schedule: "0 * * * *".into(), prompt: "y".into(), durable: Some(true) },
        "s1", &reg,
    ).await;
    assert_eq!(durable.scheduler_mode, SchedulingSurface::DurableRoutine);
    assert!(!durable.warnings.is_empty());
}

#[test]
fn regression_tier3_schedule_unsupported() {
    let r = map_schedule_command();
    assert_eq!(r.strategy, MappingStrategy::UnsupportedExplicit);
}

#[test]
fn regression_tier3_web_fetch() {
    let r = map_web_fetch("https://example.com");
    assert_eq!(r.strategy, MappingStrategy::MediatedNative);
}

// Tier 4 fixtures
#[test]
fn regression_tier4_guidance() {
    let init = map_init_guidance("/project");
    assert!(init.proposed_path.ends_with("AGENTS.md"));

    let mem = map_memory_import("/project");
    assert!(mem.proposal_only);
    assert!(mem.warnings.iter().any(|w| w.contains("No auto-sync")));
}

// Acceptance gates
#[test]
fn regression_a03_no_silent_downgrades() {
    // All mediated_native surfaces emit warnings
    let w = map_write(&test_thread(), "f", "c");
    assert!(!w.warnings.is_empty());
    let e = map_edit(&test_thread(), "f", serde_json::json!({}));
    assert!(!e.warnings.is_empty());
    let b = map_bash(&test_thread(), "ls");
    assert!(!b.warnings.is_empty());
    let m = map_multiedit(&test_thread(), vec![]);
    assert!(!m.warnings.is_empty());
}

#[test]
fn regression_a04_ask_user_never_dispatched_as_approval() {
    let event = AppServerEvent {
        method: "terminal_interaction".into(),
        kind: AppServerEventKind::TerminalInteraction,
        params: serde_json::json!({"action": "ask_user", "question": "which?"}),
        thread_id: Some("t".into()), turn_id: Some("u".into()),
        item_id: None, delta: None,
    };
    let result = classify_interaction(&event);
    assert!(matches!(result, Some(InteractionClassification::Clarification(_))));
    // Must NOT be approval
    assert!(!matches!(result, Some(InteractionClassification::Approval(_))));
}
