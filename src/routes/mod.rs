pub mod admin;
pub mod api;
mod dispatch;
use std::collections::HashMap;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde::Serialize;
use serde_json::json;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;
use warp::{http::StatusCode, Filter, Reply};

use crate::adapters::claude_output::BridgeMetadata;
use crate::app_server::{ApiStability, AppServerClient, DelegationPolicy, UserInput};
use crate::domain::anthropic::{AnthropicError, AnthropicErrorBody, AnthropicMessagesRequest};
use crate::domain::anthropic::{
    AnthropicMessagesResponse, AnthropicResponseContentBlock, AnthropicUsage,
};
use crate::domain::codex::{CodexResponsesRequest, CodexToolChoice};
use crate::domain::openai::{
    ChatCompletionsRequest, ChatCompletionsResponse, OpenAIChoice, OpenAIResponseMessage,
    OpenAIUsage,
};
use crate::jobs::{ExecutorRequest, JobCollectionError, JobExecutor, JobKind, JobRegistry};
#[cfg(test)]
use crate::mapping::approvals::SandboxConfig;
use crate::mapping::commands::{
    map_mcp_command, map_plugin_command, map_schedule_command, map_security_review_command,
    map_tasks_command, CommandResult,
};
use crate::mapping::guidance::{map_init_guidance, map_memory_import};
use crate::mapping::planning::map_plan_command;
use crate::mapping::review::ReviewRequest;
use crate::model_profiles::expand_public_models;
use crate::observability::traces::log_mapping_decision;
use crate::proxy::codex_client::{CodexClient, UpstreamError};
use crate::skills::{prepare_anthropic_request, SkillRegistry};
use crate::state::StateStore;
use crate::surfaces::{
    ClassifiedSurface, CompatibilityMatrix, OperationMode, SurfaceClassifier, SurfaceRegistry,
};
use crate::translation::anthropic_to_codex::translate_anthropic_to_codex;
use crate::translation::app_server_to_anthropic::{
    collect_app_server_to_anthropic, stream_executor_job_to_anthropic,
};
use crate::translation::app_server_to_openai::{
    collect_app_server_to_openai, stream_executor_job_to_openai,
};
use crate::translation::codex_to_anthropic::{
    collect_codex_to_anthropic, stream_codex_to_anthropic,
};
use crate::translation::codex_to_openai::{collect_codex_to_openai, stream_codex_to_openai};
use crate::translation::openai_to_codex::translate_openai_to_codex;
use crate::translation::tool_runtime::ToolRegistry;

use self::dispatch::{DispatchBackend, DispatchPlanner};

/// Cached upstream rate-limit so subsequent retries are rejected instantly.
#[derive(Clone)]
struct CachedRateLimit {
    /// The raw JSON body from the 429 response — forwarded as-is to clients.
    body: serde_json::Value,
    /// Instant after which this cache entry is considered stale.
    expires_at: std::time::Instant,
}

const MAX_RATE_LIMIT_TTL_SECS: u64 = 6 * 60 * 60;

#[derive(Clone)]
struct AppServerRuntime {
    auth_path: PathBuf,
    auth_fingerprint: Option<u64>,
    client: Arc<AppServerClient>,
    executor: Arc<JobExecutor>,
}

#[derive(Clone)]
struct AppState {
    default_auth_path: PathBuf,
    response_clients: Arc<Mutex<HashMap<PathBuf, Arc<CodexClient>>>>,
    app_server_runtime: Arc<RwLock<Option<AppServerRuntime>>>,
    skill_registry: Option<Arc<SkillRegistry>>,
    surface_registry: Arc<SurfaceRegistry>,
    compatibility_matrix: Arc<CompatibilityMatrix>,
    classifier: Arc<SurfaceClassifier>,
    job_registry: JobRegistry,
    state_store: StateStore,
    operation_mode: OperationMode,
    api_stability: ApiStability,
    delegation_policy: DelegationPolicy,
    rate_limit_guard: Arc<RwLock<Option<CachedRateLimit>>>,
    pool: Arc<crate::accounts::AccountPool>,
    account_sync_clock: Arc<Mutex<Option<std::time::Instant>>>,
}

pub struct RouteBuildOptions {
    pub client: Option<CodexClient>,
    pub app_server: Option<AppServerClient>,
    pub executor: Option<JobExecutor>,
    pub default_auth_path: String,
    pub skill_registry: Option<SkillRegistry>,
    pub surface_registry: SurfaceRegistry,
    pub compatibility_matrix: CompatibilityMatrix,
    pub job_registry: JobRegistry,
    pub state_store: StateStore,
    pub operation_mode: OperationMode,
    pub api_stability: ApiStability,
    pub delegation_policy: DelegationPolicy,
    pub pool: Arc<crate::accounts::AccountPool>,
}

pub fn build_routes(
    options: RouteBuildOptions,
) -> impl Filter<Extract = impl Reply, Error = warp::Rejection> + Clone {
    let surface_registry = Arc::new(options.surface_registry);
    let pool = options.pool.clone();
    let default_auth_path = PathBuf::from(options.default_auth_path);
    let mut response_clients = HashMap::new();
    if let Some(client) = options.client {
        response_clients.insert(default_auth_path.clone(), Arc::new(client));
    }
    let app_server_runtime = match (options.app_server, options.executor) {
        (Some(client), Some(executor)) => Some(AppServerRuntime {
            auth_path: default_auth_path.clone(),
            auth_fingerprint: crate::accounts::auth_store::auth_file_fingerprint(
                &default_auth_path,
            ),
            client: Arc::new(client),
            executor: Arc::new(executor),
        }),
        _ => None,
    };
    let state = AppState {
        default_auth_path,
        response_clients: Arc::new(Mutex::new(response_clients)),
        app_server_runtime: Arc::new(RwLock::new(app_server_runtime)),
        skill_registry: options.skill_registry.map(Arc::new),
        surface_registry: surface_registry.clone(),
        compatibility_matrix: Arc::new(options.compatibility_matrix),
        classifier: Arc::new(SurfaceClassifier::new((*surface_registry).clone())),
        job_registry: options.job_registry,
        state_store: options.state_store,
        operation_mode: options.operation_mode,
        api_stability: options.api_stability,
        delegation_policy: options.delegation_policy,
        rate_limit_guard: Arc::new(RwLock::new(None)),
        pool: pool.clone(),
        account_sync_clock: Arc::new(Mutex::new(None)),
    };
    let state_filter = warp::any().map(move || state.clone());

    let health = warp::path("health")
        .and(warp::get())
        .and_then(handle_health);

    let models_v1 = warp::path!("v1" / "models")
        .and(warp::get())
        .and(state_filter.clone())
        .and_then(handle_models);

    let models = warp::path("models")
        .and(warp::get())
        .and(warp::path::end())
        .and(state_filter.clone())
        .and_then(handle_models);

    let bridge_surfaces = warp::path!("bridge" / "surfaces")
        .and(warp::get())
        .and(state_filter.clone())
        .and_then(handle_bridge_surfaces);

    let bridge_surface = warp::path!("bridge" / "surfaces" / String)
        .and(warp::get())
        .and(state_filter.clone())
        .and_then(handle_bridge_surface);

    let bridge_compatibility = warp::path!("bridge" / "compatibility")
        .and(warp::get())
        .and(state_filter.clone())
        .and_then(handle_bridge_compatibility);

    let bridge_jobs = warp::path!("bridge" / "jobs")
        .and(warp::get())
        .and(state_filter.clone())
        .and_then(handle_bridge_jobs);

    let bridge_sessions = warp::path!("bridge" / "sessions")
        .and(warp::get())
        .and(state_filter.clone())
        .and_then(handle_bridge_sessions);

    let bridge_session = warp::path!("bridge" / "session" / String)
        .and(warp::get())
        .and(state_filter.clone())
        .and_then(handle_bridge_session);

    let bridge_mode = warp::path!("bridge" / "mode")
        .and(warp::get())
        .and(state_filter.clone())
        .and_then(handle_bridge_mode);

    let anthropic_v1 = warp::path!("v1" / "messages")
        .and(warp::post())
        .and(warp::header::headers_cloned())
        .and(warp::body::bytes())
        .and(state_filter.clone())
        .and_then(handle_anthropic_messages);

    let anthropic = warp::path("messages")
        .and(warp::post())
        .and(warp::path::end())
        .and(warp::header::headers_cloned())
        .and(warp::body::bytes())
        .and(state_filter.clone())
        .and_then(handle_anthropic_messages);

    let openai_v1 = warp::path!("v1" / "chat" / "completions")
        .and(warp::post())
        .and(warp::header::headers_cloned())
        .and(warp::body::bytes())
        .and(state_filter.clone())
        .and_then(handle_openai_chat);

    let openai = warp::path("chat")
        .and(warp::path("completions"))
        .and(warp::path::end())
        .and(warp::post())
        .and(warp::header::headers_cloned())
        .and(warp::body::bytes())
        .and(state_filter)
        .and_then(handle_openai_chat);

    let cors = warp::cors()
        .allow_any_origin()
        .allow_headers(vec![
            "authorization",
            "content-type",
            "accept",
            "accept-encoding",
            "x-api-key",
            "anthropic-version",
            "anthropic-beta",
            "x-stainless-arch",
            "x-stainless-lang",
            "x-stainless-os",
            "x-stainless-package-version",
            "x-stainless-retry-count",
            "x-stainless-runtime",
            "x-stainless-runtime-version",
            "x-stainless-timeout",
        ])
        .allow_methods(vec!["GET", "POST", "OPTIONS"]);

    let api_routes = crate::routes::api::api_routes(pool);
    let admin_ui_routes = crate::routes::admin::admin_routes();

    health
        .or(models_v1)
        .or(models)
        .or(bridge_surfaces)
        .or(bridge_surface)
        .or(bridge_compatibility)
        .or(bridge_jobs)
        .or(bridge_sessions)
        .or(bridge_session)
        .or(bridge_mode)
        .or(anthropic_v1)
        .or(anthropic)
        .or(openai_v1)
        .or(openai)
        .or(api_routes)
        .or(admin_ui_routes)
        .with(cors)
        .with(warp::log("codex_proxy"))
}

async fn handle_health() -> Result<impl Reply, warp::Rejection> {
    Ok(warp::reply::json(&json!({
        "status": "ok",
        "service": "codex-openai-proxy"
    })))
}

async fn handle_models(state: AppState) -> Result<impl Reply, warp::Rejection> {
    let models = list_public_models(&state).await;

    let data: Vec<serde_json::Value> = models
        .into_iter()
        .map(|id| {
            json!({
                "id": id,
                "object": "model",
                "created": 1687882411,
                "owned_by": "openai"
            })
        })
        .collect();

    Ok(warp::reply::json(&json!({
        "object": "list",
        "data": data
    })))
}

async fn list_legacy_models(state: &AppState) -> Vec<String> {
    match response_client_for_auth_path(state, &state.default_auth_path).await {
        Some(client) => client.list_models().await,
        None => vec!["gpt-5.2-codex".to_string()],
    }
}

async fn list_public_models(state: &AppState) -> Vec<String> {
    let mut models = Vec::new();

    if !matches!(state.operation_mode, OperationMode::ResponsesOnly) {
        if let Ok(Some(runtime)) = ensure_app_server_runtime(state, &state.default_auth_path).await
        {
            if let Ok(app_server_models) = runtime.client.model_list().await {
                models.extend(app_server_models.into_iter().map(|model| model.id));
            }
        }
    }

    if matches!(
        state.operation_mode,
        OperationMode::AutoHybrid | OperationMode::ResponsesOnly
    ) {
        models.extend(list_legacy_models(state).await);
    }

    if models.is_empty() {
        models.push("gpt-5.2-codex".to_string());
    }

    expand_public_models(models)
}

fn account_auto_sync_enabled() -> bool {
    std::env::var("CLAUDE_CODEX_PROXY_ACCOUNT_AUTO_SYNC")
        .map(|value| !matches!(value.as_str(), "0" | "false" | "FALSE" | "False"))
        .unwrap_or(true)
}

fn account_auto_sync_interval() -> Duration {
    let secs = std::env::var("CLAUDE_CODEX_PROXY_ACCOUNT_AUTO_SYNC_INTERVAL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30);
    Duration::from_secs(secs.max(1))
}

async fn maybe_auto_sync_accounts(state: &AppState) {
    if !account_auto_sync_enabled() {
        return;
    }

    let now = std::time::Instant::now();
    let interval = account_auto_sync_interval();
    {
        let mut guard = state.account_sync_clock.lock().await;
        if guard
            .as_ref()
            .map(|last| now.duration_since(*last) < interval)
            .unwrap_or(false)
        {
            return;
        }
        *guard = Some(now);
    }

    if let Err(error) = state.pool.sync_discovered().await {
        log::debug!("[account_pool] automatic sync skipped: {}", error);
    }
}

async fn select_request_auth_path(state: &AppState) -> Option<PathBuf> {
    maybe_auto_sync_accounts(state).await;
    let preferred = state
        .app_server_runtime
        .read()
        .await
        .as_ref()
        .map(|runtime| runtime.auth_path.clone())
        .or_else(|| Some(state.default_auth_path.clone()));

    state.pool.preferred_auth_path(preferred.as_deref()).await
}

async fn account_id_for_auth_path(state: &AppState, auth_path: &Path) -> Option<String> {
    state.pool.account_id_for_auth_path(auth_path).await
}

fn extract_client_session_id(headers: &warp::http::HeaderMap) -> Option<String> {
    const CANDIDATES: &[&str] = &[
        "x-claude-session-id",
        "x-session-id",
        "session-id",
        "session_id",
    ];

    CANDIDATES.iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn latest_anthropic_assistant_text_for_affinity(
    request: &AnthropicMessagesRequest,
) -> Option<String> {
    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant")
        .and_then(flatten_anthropic_message_for_app_server)
}

fn latest_openai_assistant_text_for_affinity(request: &ChatCompletionsRequest) -> Option<String> {
    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant")
        .and_then(flatten_openai_message_for_app_server)
}

async fn find_session_by_client_affinity(
    state: &AppState,
    client_session_id: Option<&str>,
    last_assistant_message: Option<&str>,
) -> Option<crate::app_server::BridgeSession> {
    let sessions = state.state_store.list_sessions().await;

    if let Some(client_session_id) = client_session_id {
        if let Some(session) = sessions
            .iter()
            .filter(|session| session.claude_session_id.as_deref() == Some(client_session_id))
            .max_by(|left, right| compare_session_recency(left, right))
        {
            return Some(session.clone());
        }
    }

    let last_assistant_message = last_assistant_message?;
    sessions
        .into_iter()
        .filter(|session| session.last_assistant_message.as_deref() == Some(last_assistant_message))
        .max_by(compare_session_recency)
}

fn compare_session_recency(
    left: &crate::app_server::BridgeSession,
    right: &crate::app_server::BridgeSession,
) -> Ordering {
    left.thread
        .created_at_unix
        .cmp(&right.thread.created_at_unix)
        .then_with(|| left.thread.turn_count.cmp(&right.thread.turn_count))
        .then_with(|| left.bridge_session_id.cmp(&right.bridge_session_id))
}

async fn resolve_anthropic_continuation_session(
    state: &AppState,
    headers: &warp::http::HeaderMap,
    request: &AnthropicMessagesRequest,
) -> Option<crate::app_server::BridgeSession> {
    find_session_by_client_affinity(
        state,
        extract_client_session_id(headers).as_deref(),
        latest_anthropic_assistant_text_for_affinity(request).as_deref(),
    )
    .await
}

async fn resolve_openai_continuation_session(
    state: &AppState,
    headers: &warp::http::HeaderMap,
    request: &ChatCompletionsRequest,
) -> Option<crate::app_server::BridgeSession> {
    find_session_by_client_affinity(
        state,
        extract_client_session_id(headers).as_deref(),
        latest_openai_assistant_text_for_affinity(request).as_deref(),
    )
    .await
}

async fn response_client_for_auth_path(
    state: &AppState,
    auth_path: &Path,
) -> Option<Arc<CodexClient>> {
    let auth_path = auth_path.to_path_buf();

    {
        let guard = state.response_clients.lock().await;
        if let Some(client) = guard.get(&auth_path) {
            return Some(client.clone());
        }
    }

    match CodexClient::from_auth_path(&auth_path.to_string_lossy()).await {
        Ok(client) => {
            let client = Arc::new(client);
            let mut guard = state.response_clients.lock().await;
            let entry = guard.entry(auth_path).or_insert_with(|| client.clone());
            Some(entry.clone())
        }
        Err(error) => {
            log::warn!(
                "responses client unavailable for auth_path={}: {}",
                auth_path.display(),
                error
            );
            None
        }
    }
}

async fn ensure_app_server_runtime(
    state: &AppState,
    auth_path: &Path,
) -> anyhow::Result<Option<AppServerRuntime>> {
    if matches!(state.operation_mode, OperationMode::ResponsesOnly) {
        return Ok(None);
    }

    let auth_fingerprint = crate::accounts::auth_store::auth_file_fingerprint(auth_path);

    {
        let guard = state.app_server_runtime.read().await;
        if let Some(runtime) = guard.as_ref() {
            if runtime.auth_path == auth_path && runtime.auth_fingerprint == auth_fingerprint {
                return Ok(Some(runtime.clone()));
            }
        }
    }

    let code_home = auth_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let client = AppServerClient::connect(crate::app_server::AppServerConnectOptions {
        api_stability: state.api_stability,
        extra_env: vec![("CODEX_HOME".to_string(), code_home.display().to_string())],
        ..crate::app_server::AppServerConnectOptions::default()
    })
    .await?;
    let executor = JobExecutor::with_runtime(
        client.clone(),
        state.job_registry.clone(),
        state.state_store.clone(),
        state.operation_mode,
        state.api_stability,
        state.delegation_policy.clone(),
        Some(state.pool.clone()),
    );
    let runtime = AppServerRuntime {
        auth_path: auth_path.to_path_buf(),
        auth_fingerprint,
        client: Arc::new(client),
        executor: Arc::new(executor),
    };

    let mut guard = state.app_server_runtime.write().await;
    *guard = Some(runtime.clone());
    Ok(Some(runtime))
}

async fn no_available_account_openai(state: &AppState) -> warp::reply::Response {
    let mut response = openai_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "No enabled account is currently available",
        "account_unavailable",
    );
    if let Some(retry_after) = state.pool.soonest_reset_secs().await {
        if let Ok(header_value) = retry_after.to_string().parse() {
            response
                .headers_mut()
                .insert(warp::http::header::RETRY_AFTER, header_value);
        }
    }
    response
}

async fn no_available_account_anthropic(state: &AppState) -> warp::reply::Response {
    let mut response = anthropic_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "api_error",
        "No enabled account is currently available",
    );
    if let Some(retry_after) = state.pool.soonest_reset_secs().await {
        if let Ok(header_value) = retry_after.to_string().parse() {
            response
                .headers_mut()
                .insert(warp::http::header::RETRY_AFTER, header_value);
        }
    }
    response
}

fn app_server_terminal_error(events: &[crate::app_server::AppServerEvent]) -> Option<String> {
    events.iter().find_map(|event| match event.kind {
        crate::app_server::AppServerEventKind::Error => event.error_message(),
        _ => None,
    })
}

fn app_server_events_have_user_visible_output(
    events: &[crate::app_server::AppServerEvent],
) -> bool {
    events.iter().any(|event| match event.kind {
        crate::app_server::AppServerEventKind::AgentMessageDelta => event
            .delta
            .as_deref()
            .map(|delta| !delta.is_empty())
            .unwrap_or(false),
        crate::app_server::AppServerEventKind::ItemStarted
        | crate::app_server::AppServerEventKind::ItemCompleted => {
            event.item_type() == Some("function_call")
        }
        _ => false,
    })
}

async fn handle_openai_chat(
    headers: warp::http::HeaderMap,
    body: Bytes,
    state: AppState,
) -> Result<warp::reply::Response, warp::Rejection> {
    let trace_id = Uuid::new_v4().to_string();

    let request: ChatCompletionsRequest = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[{trace_id}] invalid openai json: {e}");
            return Ok(openai_error(
                StatusCode::BAD_REQUEST,
                "Invalid JSON body",
                "invalid_request_error",
            ));
        }
    };

    log_request_summary(
        &trace_id,
        "openai",
        request.model.as_str(),
        request.messages.len(),
        request.stream.unwrap_or(false),
        request.tools.as_ref().map(|v| v.len()).unwrap_or_default(),
        &headers,
    );

    let classified_surfaces = state.classifier.classify_openai_request(&request);
    log_surface_summary(&trace_id, &classified_surfaces);
    log_surface_decisions(&state, &classified_surfaces);
    let response_bridge = primary_bridge_metadata(&state, &classified_surfaces);
    let client_session_id = extract_client_session_id(&headers);
    let continuation_session = resolve_openai_continuation_session(&state, &headers, &request)
        .await
        .filter(|session| session.account_auth_path.is_some());
    let continuation_prompt = continuation_session
        .as_ref()
        .and_then(|_| latest_openai_user_turn_for_app_server(&request));
    let continuation_session = continuation_session.filter(|_| continuation_prompt.is_some());
    let continuation_auth_path = continuation_session
        .as_ref()
        .and_then(|session| session.account_auth_path.as_deref())
        .map(PathBuf::from);
    let selected_auth_path = if let Some(path) = continuation_auth_path.clone() {
        path
    } else {
        let Some(path) = select_request_auth_path(&state).await else {
            return Ok(no_available_account_openai(&state).await);
        };
        path
    };
    let selected_account_id = if let Some(account_id) = continuation_session
        .as_ref()
        .and_then(|session| session.account_id.clone())
    {
        Some(account_id)
    } else {
        account_id_for_auth_path(&state, &selected_auth_path).await
    };
    let app_server_runtime = match ensure_app_server_runtime(&state, &selected_auth_path).await {
        Ok(runtime) => runtime,
        Err(err) if matches!(state.operation_mode, OperationMode::AutoHybrid) => {
            log::warn!(
                "[{trace_id}] app-server unavailable for auth_path={}: {}",
                selected_auth_path.display(),
                err
            );
            state
                .pool
                .report_error(&selected_auth_path, &err.to_string())
                .await;
            None
        }
        Err(err) => {
            return Ok(openai_error(
                StatusCode::BAD_GATEWAY,
                &format!("App-server request failed: {err}"),
                "app_server_error",
            ));
        }
    };
    let dispatch_plan = DispatchPlanner::plan_openai(
        &request,
        &classified_surfaces,
        state.operation_mode,
        app_server_runtime.is_some(),
        &state.compatibility_matrix,
    );

    if let Some(response) = try_openai_local_command(&state, &request).await {
        return Ok(response);
    }

    if matches!(dispatch_plan.backend, DispatchBackend::ResponsesFallback) {
        if let Some(cached_body) = check_rate_limit_guard(&state).await {
            log::info!("[{trace_id}] rate-limit guard active for responses fallback");
            return Ok(warp::reply::with_status(
                warp::reply::json(&cached_body),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .into_response());
        }
    }

    if matches!(dispatch_plan.backend, DispatchBackend::AppServer) {
        let prompt = continuation_prompt.or_else(|| build_openai_app_server_prompt(&request));
        if let (Some(runtime), Some(prompt)) = (app_server_runtime.as_ref(), prompt) {
            let using_continuation = continuation_session.is_some();
            let executor_request = ExecutorRequest {
                origin_surface_id: primary_surface_id(&classified_surfaces)
                    .unwrap_or_else(|| "openai.chat.completions".to_string()),
                kind: job_kind_for_surfaces(&classified_surfaces),
                cwd: std::env::current_dir()
                    .ok()
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_else(|| ".".to_string()),
                model: request.model.clone(),
                developer_instructions: None,
                input: vec![UserInput::Text { text: prompt }],
                existing_thread_id: continuation_session
                    .as_ref()
                    .map(|session| session.thread.thread_id.clone()),
                client_session_id: client_session_id.clone(),
                account_id: selected_account_id.clone(),
                account_auth_path: Some(selected_auth_path.display().to_string()),
            };
            let tool_registry = ToolRegistry::from_openai_request(&request);
            let executor = runtime.executor.clone();
            let mut responses_fallback_auth_path = selected_auth_path.clone();

            match dispatch_plan.execution_mode {
                self::dispatch::ExecutionMode::AttachedStream => {
                    match executor.start_job(executor_request).await {
                        Ok(start) => {
                            let Some(rx) = executor.subscribe(&start.job_id).await else {
                                return Ok(openai_error(
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    "Executor stream subscription is unavailable",
                                    "bridge_unavailable",
                                ));
                            };
                            let stream = stream_executor_job_to_openai(
                                rx,
                                format!("chatcmpl-{}", Uuid::new_v4()),
                                request.model.clone(),
                            );
                            let sse = warp::sse::reply(warp::sse::keep_alive().stream(stream));
                            let sse = warp::reply::with_header(sse, "cache-control", "no-cache");
                            let sse = warp::reply::with_header(sse, "x-accel-buffering", "no");
                            return Ok(sse.into_response());
                        }
                        Err(err) if matches!(state.operation_mode, OperationMode::AutoHybrid) => {
                            state
                                .pool
                                .report_error(&selected_auth_path, &err.to_string())
                                .await;
                            if using_continuation
                                && is_recoverable_continuation_error(&err.to_string())
                            {
                                if let Some(path) = select_request_auth_path(&state).await {
                                    responses_fallback_auth_path = path;
                                }
                            }
                            log::warn!("[{trace_id}] executor openai stream path failed, fallback to responses: {err}");
                        }
                        Err(err) => {
                            state
                                .pool
                                .report_error(&selected_auth_path, &err.to_string())
                                .await;
                            return Ok(openai_error(
                                StatusCode::BAD_GATEWAY,
                                &format!("App-server request failed: {err}"),
                                "app_server_error",
                            ));
                        }
                    }
                }
                self::dispatch::ExecutionMode::AttachedCollect => {
                    match executor.start_job(executor_request).await {
                        Ok(start) => {
                            match executor
                                .collect_until_complete(&start.job_id, app_server_turn_timeout())
                                .await
                            {
                                Ok(events) => {
                                    if let Some(error) = app_server_terminal_error(&events) {
                                        if using_continuation
                                            && matches!(
                                                state.operation_mode,
                                                OperationMode::AutoHybrid
                                            )
                                            && is_recoverable_continuation_error(&error)
                                        {
                                            state
                                                .pool
                                                .report_error(&selected_auth_path, &error)
                                                .await;
                                            if let Some(path) =
                                                select_request_auth_path(&state).await
                                            {
                                                responses_fallback_auth_path = path;
                                            }
                                            log::warn!(
                                                "[{trace_id}] recoverable continuation error on openai collect path, falling back to responses: {error}"
                                            );
                                            return handle_openai_via_responses(
                                                trace_id.clone(),
                                                headers.clone(),
                                                request.clone(),
                                                state.clone(),
                                                response_bridge.clone(),
                                                responses_fallback_auth_path,
                                            )
                                            .await;
                                        } else {
                                            return Ok(openai_error(
                                                StatusCode::BAD_GATEWAY,
                                                &format!("App-server request failed: {error}"),
                                                "app_server_error",
                                            ));
                                        }
                                    }
                                    if !app_server_events_have_user_visible_output(&events) {
                                        return Ok(openai_error(
                                            StatusCode::BAD_GATEWAY,
                                            "Codex returned an empty response",
                                            "app_server_empty_response",
                                        ));
                                    }
                                    let payload = collect_app_server_to_openai(
                                        &format!("chatcmpl-{}", Uuid::new_v4()),
                                        &request.model,
                                        &events,
                                        tool_registry,
                                    );
                                    return Ok(json_response_with_bridge(
                                        &payload,
                                        response_bridge.as_ref(),
                                    ));
                                }
                                Err(JobCollectionError::Timeout) => {
                                    return Ok(openai_error(
                                        StatusCode::GATEWAY_TIMEOUT,
                                        "App-server turn timed out",
                                        "app_server_timeout",
                                    ));
                                }
                                Err(JobCollectionError::NotFound)
                                    if matches!(
                                        state.operation_mode,
                                        OperationMode::AutoHybrid
                                    ) =>
                                {
                                    log::warn!("[{trace_id}] executor lost openai job before collection; falling back to responses");
                                }
                                Err(JobCollectionError::NotFound) => {
                                    return Ok(openai_error(
                                        StatusCode::BAD_GATEWAY,
                                        "Executor job was not found",
                                        "app_server_error",
                                    ));
                                }
                            }
                        }
                        Err(err) if matches!(state.operation_mode, OperationMode::AutoHybrid) => {
                            state
                                .pool
                                .report_error(&selected_auth_path, &err.to_string())
                                .await;
                            if using_continuation
                                && is_recoverable_continuation_error(&err.to_string())
                            {
                                if let Some(path) = select_request_auth_path(&state).await {
                                    responses_fallback_auth_path = path;
                                }
                            }
                            log::warn!("[{trace_id}] executor openai collect path failed, fallback to responses: {err}");
                        }
                        Err(err) => {
                            state
                                .pool
                                .report_error(&selected_auth_path, &err.to_string())
                                .await;
                            return Ok(openai_error(
                                StatusCode::BAD_GATEWAY,
                                &format!("App-server request failed: {err}"),
                                "app_server_error",
                            ));
                        }
                    }
                }
                self::dispatch::ExecutionMode::DetachedBackground => {
                    match executor.start_job(executor_request).await {
                        Ok(start) => {
                            let body = format!("Background job started: {}", start.job_id);
                            let payload = make_openai_background_ack(&request.model, &body);
                            return Ok(json_response_with_bridge(
                                &payload,
                                response_bridge.as_ref(),
                            ));
                        }
                        Err(err) if matches!(state.operation_mode, OperationMode::AutoHybrid) => {
                            state
                                .pool
                                .report_error(&selected_auth_path, &err.to_string())
                                .await;
                            if using_continuation
                                && is_recoverable_continuation_error(&err.to_string())
                            {
                                if let Some(path) = select_request_auth_path(&state).await {
                                    responses_fallback_auth_path = path;
                                }
                            }
                            log::warn!("[{trace_id}] executor openai background path failed, fallback to responses: {err}");
                        }
                        Err(err) => {
                            state
                                .pool
                                .report_error(&selected_auth_path, &err.to_string())
                                .await;
                            return Ok(openai_error(
                                StatusCode::BAD_GATEWAY,
                                &format!("App-server request failed: {err}"),
                                "app_server_error",
                            ));
                        }
                    }
                }
            }

            return handle_openai_via_responses(
                trace_id,
                headers,
                request,
                state,
                response_bridge,
                responses_fallback_auth_path,
            )
            .await;
        }
    }

    handle_openai_via_responses(
        trace_id,
        headers,
        request,
        state,
        response_bridge,
        selected_auth_path,
    )
    .await
}

async fn handle_openai_via_responses(
    trace_id: String,
    _headers: warp::http::HeaderMap,
    request: ChatCompletionsRequest,
    state: AppState,
    bridge: Option<BridgeMetadata>,
    selected_auth_path: PathBuf,
) -> Result<warp::reply::Response, warp::Rejection> {
    let Some(client) = response_client_for_auth_path(&state, &selected_auth_path).await else {
        return Ok(openai_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Responses API fallback is unavailable",
            "bridge_unavailable",
        ));
    };

    let tool_registry = ToolRegistry::from_openai_request(&request);
    let codex_request = translate_openai_to_codex(&request);
    let has_tools = codex_request
        .tools
        .as_ref()
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    let response = match request_with_tool_fallback(
        client.as_ref(),
        codex_request,
        has_tools,
        &trace_id,
    )
    .await
    {
        Ok(v) => v,
        Err(UpstreamError::Upstream { status, body }) => {
            log::warn!(
                "[{trace_id}] upstream failed ({status}): {}",
                truncate(&body, 240)
            );

            if status == StatusCode::TOO_MANY_REQUESTS {
                cache_rate_limit(&state, &body, Some(&selected_auth_path)).await;
            } else {
                state.pool.report_error(&selected_auth_path, &body).await;
            }

            let reply_response =
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) {
                    warp::reply::with_status(warp::reply::json(&parsed), status).into_response()
                } else {
                    openai_error(status, "Upstream error", "upstream_error")
                };
            return Ok(reply_response);
        }
        Err(UpstreamError::Transport(e)) => {
            state
                .pool
                .report_error(&selected_auth_path, &e.to_string())
                .await;
            log::error!("[{trace_id}] transport error: {e}");
            return Ok(openai_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal proxy error",
                "internal_error",
            ));
        }
    };
    state.pool.report_success(&selected_auth_path).await;

    if request.stream.unwrap_or(false) {
        let stream = stream_codex_to_openai(response, request.model, trace_id, tool_registry);
        let sse = warp::sse::reply(warp::sse::keep_alive().stream(stream));
        let sse = warp::reply::with_header(sse, "cache-control", "no-cache");
        let sse = warp::reply::with_header(sse, "x-accel-buffering", "no");
        return Ok(sse.into_response());
    }

    match collect_codex_to_openai(response, request.model, &trace_id, tool_registry).await {
        Ok(payload) => Ok(json_response_with_bridge(&payload, bridge.as_ref())),
        Err(e) => {
            log::warn!("[{trace_id}] collect error: {e}");
            Ok(openai_error(
                StatusCode::BAD_GATEWAY,
                "Failed to decode upstream response",
                "bad_gateway",
            ))
        }
    }
}

async fn handle_anthropic_messages(
    headers: warp::http::HeaderMap,
    body: Bytes,
    state: AppState,
) -> Result<warp::reply::Response, warp::Rejection> {
    let trace_id = Uuid::new_v4().to_string();

    let request: AnthropicMessagesRequest = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[{trace_id}] invalid anthropic json: {e}");
            return Ok(anthropic_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Invalid JSON body",
            ));
        }
    };

    log_request_summary(
        &trace_id,
        "anthropic",
        request.model.as_str(),
        request.messages.len(),
        request.stream.unwrap_or(false),
        request.tools.as_ref().map(|v| v.len()).unwrap_or_default(),
        &headers,
    );

    let classified_surfaces = state.classifier.classify_anthropic_request(&request);
    log_surface_summary(&trace_id, &classified_surfaces);
    log_surface_decisions(&state, &classified_surfaces);
    let response_bridge = primary_bridge_metadata(&state, &classified_surfaces);
    let client_session_id = extract_client_session_id(&headers);
    let continuation_session = resolve_anthropic_continuation_session(&state, &headers, &request)
        .await
        .filter(|session| session.account_auth_path.is_some());
    let continuation_prompt = continuation_session
        .as_ref()
        .and_then(|_| latest_anthropic_user_turn_for_app_server(&request));
    let continuation_session = continuation_session.filter(|_| continuation_prompt.is_some());
    let continuation_auth_path = continuation_session
        .as_ref()
        .and_then(|session| session.account_auth_path.as_deref())
        .map(PathBuf::from);
    let selected_auth_path = if let Some(path) = continuation_auth_path.clone() {
        path
    } else {
        let Some(path) = select_request_auth_path(&state).await else {
            return Ok(no_available_account_anthropic(&state).await);
        };
        path
    };
    let selected_account_id = if let Some(account_id) = continuation_session
        .as_ref()
        .and_then(|session| session.account_id.clone())
    {
        Some(account_id)
    } else {
        account_id_for_auth_path(&state, &selected_auth_path).await
    };
    let app_server_runtime = match ensure_app_server_runtime(&state, &selected_auth_path).await {
        Ok(runtime) => runtime,
        Err(err) if matches!(state.operation_mode, OperationMode::AutoHybrid) => {
            log::warn!(
                "[{trace_id}] app-server unavailable for auth_path={}: {}",
                selected_auth_path.display(),
                err
            );
            state
                .pool
                .report_error(&selected_auth_path, &err.to_string())
                .await;
            None
        }
        Err(err) => {
            return Ok(anthropic_error(
                StatusCode::BAD_GATEWAY,
                "api_error",
                &format!("App-server request failed: {err}"),
            ));
        }
    };
    let dispatch_plan = DispatchPlanner::plan_anthropic(
        &request,
        &classified_surfaces,
        state.operation_mode,
        app_server_runtime.is_some(),
        &state.compatibility_matrix,
    );

    if let Some(response) = try_anthropic_local_command(&state, &request).await {
        return Ok(response);
    }

    if matches!(dispatch_plan.backend, DispatchBackend::ResponsesFallback) {
        if let Some(cached_body) = check_rate_limit_guard(&state).await {
            log::info!("[{trace_id}] rate-limit guard active for responses fallback");
            return Ok(warp::reply::with_status(
                warp::reply::json(&cached_body),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .into_response());
        }
    }

    if matches!(dispatch_plan.backend, DispatchBackend::AppServer) {
        let prompt = continuation_prompt
            .map(|prompt| (None, prompt))
            .or_else(|| build_anthropic_app_server_prompt(&request));
        if let (Some(runtime), Some((system_prompt, prompt))) =
            (app_server_runtime.as_ref(), prompt)
        {
            let using_continuation = continuation_session.is_some();
            let executor_request = ExecutorRequest {
                origin_surface_id: primary_surface_id(&classified_surfaces)
                    .unwrap_or_else(|| "anthropic.messages".to_string()),
                kind: job_kind_for_surfaces(&classified_surfaces),
                cwd: std::env::current_dir()
                    .ok()
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_else(|| ".".to_string()),
                model: request.model.clone(),
                developer_instructions: system_prompt,
                input: vec![UserInput::Text { text: prompt }],
                existing_thread_id: continuation_session
                    .as_ref()
                    .map(|session| session.thread.thread_id.clone()),
                client_session_id: client_session_id.clone(),
                account_id: selected_account_id.clone(),
                account_auth_path: Some(selected_auth_path.display().to_string()),
            };
            let tool_registry = ToolRegistry::from_anthropic_request(&request, None);
            let executor = runtime.executor.clone();
            let mut responses_fallback_auth_path = selected_auth_path.clone();

            match dispatch_plan.execution_mode {
                self::dispatch::ExecutionMode::AttachedStream => {
                    match executor.start_job(executor_request).await {
                        Ok(start) => {
                            let Some(rx) = executor.subscribe(&start.job_id).await else {
                                return Ok(anthropic_error(
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    "api_error",
                                    "Executor stream subscription is unavailable",
                                ));
                            };
                            let stream = stream_executor_job_to_anthropic(
                                rx,
                                format!("msg_{}", Uuid::new_v4().simple()),
                                request.model.clone(),
                            );
                            let sse = warp::sse::reply(warp::sse::keep_alive().stream(stream));
                            let sse = warp::reply::with_header(sse, "cache-control", "no-cache");
                            let sse = warp::reply::with_header(sse, "x-accel-buffering", "no");
                            return Ok(sse.into_response());
                        }
                        Err(err) if matches!(state.operation_mode, OperationMode::AutoHybrid) => {
                            state
                                .pool
                                .report_error(&selected_auth_path, &err.to_string())
                                .await;
                            if using_continuation
                                && is_recoverable_continuation_error(&err.to_string())
                            {
                                if let Some(path) = select_request_auth_path(&state).await {
                                    responses_fallback_auth_path = path;
                                }
                            }
                            log::warn!("[{trace_id}] executor anthropic stream path failed, fallback to responses: {err}");
                        }
                        Err(err) => {
                            state
                                .pool
                                .report_error(&selected_auth_path, &err.to_string())
                                .await;
                            return Ok(anthropic_error(
                                StatusCode::BAD_GATEWAY,
                                "api_error",
                                &format!("App-server request failed: {err}"),
                            ));
                        }
                    }
                }
                self::dispatch::ExecutionMode::AttachedCollect => {
                    match executor.start_job(executor_request).await {
                        Ok(start) => {
                            match executor
                                .collect_until_complete(&start.job_id, app_server_turn_timeout())
                                .await
                            {
                                Ok(events) => {
                                    if let Some(error) = app_server_terminal_error(&events) {
                                        if using_continuation
                                            && matches!(
                                                state.operation_mode,
                                                OperationMode::AutoHybrid
                                            )
                                            && is_recoverable_continuation_error(&error)
                                        {
                                            state
                                                .pool
                                                .report_error(&selected_auth_path, &error)
                                                .await;
                                            if let Some(path) =
                                                select_request_auth_path(&state).await
                                            {
                                                responses_fallback_auth_path = path;
                                            }
                                            log::warn!(
                                                "[{trace_id}] recoverable continuation error on anthropic collect path, falling back to responses: {error}"
                                            );
                                            return handle_anthropic_via_responses(
                                                trace_id.clone(),
                                                request.clone(),
                                                state.clone(),
                                                response_bridge.clone(),
                                                responses_fallback_auth_path,
                                            )
                                            .await;
                                        } else {
                                            return Ok(anthropic_error(
                                                StatusCode::BAD_GATEWAY,
                                                "api_error",
                                                &format!("App-server request failed: {error}"),
                                            ));
                                        }
                                    }
                                    if !app_server_events_have_user_visible_output(&events) {
                                        return Ok(anthropic_error(
                                            StatusCode::BAD_GATEWAY,
                                            "api_error",
                                            "Codex returned an empty response",
                                        ));
                                    }
                                    let payload = collect_app_server_to_anthropic(
                                        &format!("msg_{}", Uuid::new_v4().simple()),
                                        &request.model,
                                        &events,
                                        tool_registry,
                                    );
                                    return Ok(json_response_with_bridge(
                                        &payload,
                                        response_bridge.as_ref(),
                                    ));
                                }
                                Err(JobCollectionError::Timeout) => {
                                    return Ok(anthropic_error(
                                        StatusCode::GATEWAY_TIMEOUT,
                                        "api_error",
                                        "App-server turn timed out",
                                    ));
                                }
                                Err(JobCollectionError::NotFound)
                                    if matches!(
                                        state.operation_mode,
                                        OperationMode::AutoHybrid
                                    ) =>
                                {
                                    log::warn!("[{trace_id}] executor lost anthropic job before collection; falling back to responses");
                                }
                                Err(JobCollectionError::NotFound) => {
                                    return Ok(anthropic_error(
                                        StatusCode::BAD_GATEWAY,
                                        "api_error",
                                        "Executor job was not found",
                                    ));
                                }
                            }
                        }
                        Err(err) if matches!(state.operation_mode, OperationMode::AutoHybrid) => {
                            state
                                .pool
                                .report_error(&selected_auth_path, &err.to_string())
                                .await;
                            if using_continuation
                                && is_recoverable_continuation_error(&err.to_string())
                            {
                                if let Some(path) = select_request_auth_path(&state).await {
                                    responses_fallback_auth_path = path;
                                }
                            }
                            log::warn!("[{trace_id}] executor anthropic collect path failed, fallback to responses: {err}");
                        }
                        Err(err) => {
                            state
                                .pool
                                .report_error(&selected_auth_path, &err.to_string())
                                .await;
                            return Ok(anthropic_error(
                                StatusCode::BAD_GATEWAY,
                                "api_error",
                                &format!("App-server request failed: {err}"),
                            ));
                        }
                    }
                }
                self::dispatch::ExecutionMode::DetachedBackground => {
                    match executor.start_job(executor_request).await {
                        Ok(start) => {
                            let body = format!("Background job started: {}", start.job_id);
                            let payload = make_background_ack(&request.model, &body);
                            return Ok(json_response_with_bridge(
                                &payload,
                                response_bridge.as_ref(),
                            ));
                        }
                        Err(err) if matches!(state.operation_mode, OperationMode::AutoHybrid) => {
                            state
                                .pool
                                .report_error(&selected_auth_path, &err.to_string())
                                .await;
                            if using_continuation
                                && is_recoverable_continuation_error(&err.to_string())
                            {
                                if let Some(path) = select_request_auth_path(&state).await {
                                    responses_fallback_auth_path = path;
                                }
                            }
                            log::warn!("[{trace_id}] executor anthropic background path failed, fallback to responses: {err}");
                        }
                        Err(err) => {
                            state
                                .pool
                                .report_error(&selected_auth_path, &err.to_string())
                                .await;
                            return Ok(anthropic_error(
                                StatusCode::BAD_GATEWAY,
                                "api_error",
                                &format!("App-server request failed: {err}"),
                            ));
                        }
                    }
                }
            }

            return handle_anthropic_via_responses(
                trace_id,
                request,
                state,
                response_bridge,
                responses_fallback_auth_path,
            )
            .await;
        }
    }

    handle_anthropic_via_responses(
        trace_id,
        request,
        state,
        response_bridge,
        selected_auth_path,
    )
    .await
}

async fn handle_anthropic_via_responses(
    trace_id: String,
    request: AnthropicMessagesRequest,
    state: AppState,
    bridge: Option<BridgeMetadata>,
    selected_auth_path: PathBuf,
) -> Result<warp::reply::Response, warp::Rejection> {
    let Some(client) = response_client_for_auth_path(&state, &selected_auth_path).await else {
        return Ok(anthropic_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "api_error",
            "Responses API fallback is unavailable",
        ));
    };

    let prepared_request = prepare_anthropic_request(
        &request,
        state.skill_registry.as_ref().map(Arc::as_ref),
        &trace_id,
    );
    match (
        &prepared_request.requested_marker,
        prepared_request.bridge.as_ref(),
    ) {
        (Some(_marker), Some(bridge)) => {
            log::info!(
                "[{trace_id}] resolved skill marker={} skill={} version={} tool_aliases={} references={}",
                bridge.marker,
                bridge.id,
                bridge.version,
                bridge.tool_aliases.len(),
                bridge.references.len()
            );
        }
        (Some(marker), None) => {
            log::warn!("[{trace_id}] unresolved skill marker={marker}; continuing without bridge");
        }
        (None, _) => {}
    }

    let tool_registry = ToolRegistry::from_anthropic_request(
        &prepared_request.request,
        prepared_request
            .bridge
            .as_ref()
            .map(|bridge| &bridge.tool_aliases),
    );
    let codex_request =
        translate_anthropic_to_codex(&prepared_request.request, prepared_request.bridge.as_ref());
    let has_tools = codex_request
        .tools
        .as_ref()
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    let response = match request_with_tool_fallback(
        client.as_ref(),
        codex_request,
        has_tools,
        &trace_id,
    )
    .await
    {
        Ok(v) => v,
        Err(UpstreamError::Upstream { status, body }) => {
            log::warn!(
                "[{trace_id}] upstream failed ({status}): {}",
                truncate(&body, 240)
            );

            if status == StatusCode::TOO_MANY_REQUESTS {
                cache_rate_limit(&state, &body, Some(&selected_auth_path)).await;
            } else {
                state.pool.report_error(&selected_auth_path, &body).await;
            }

            let reply_response =
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) {
                    warp::reply::with_status(warp::reply::json(&parsed), status).into_response()
                } else {
                    anthropic_error(
                        status,
                        anthropic_error_type_for_status(status),
                        anthropic_error_message_for_status(status),
                    )
                };
            return Ok(reply_response);
        }
        Err(UpstreamError::Transport(e)) => {
            state
                .pool
                .report_error(&selected_auth_path, &e.to_string())
                .await;
            log::error!("[{trace_id}] transport error: {e}");
            return Ok(anthropic_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                "Internal proxy error",
            ));
        }
    };
    state.pool.report_success(&selected_auth_path).await;

    if prepared_request.request.stream.unwrap_or(false) {
        let stream = stream_codex_to_anthropic(
            response,
            prepared_request.request.model.clone(),
            trace_id,
            tool_registry,
        );
        let sse = warp::sse::reply(warp::sse::keep_alive().stream(stream));
        let sse = warp::reply::with_header(sse, "cache-control", "no-cache");
        let sse = warp::reply::with_header(sse, "x-accel-buffering", "no");
        return Ok(sse.into_response());
    }

    match collect_codex_to_anthropic(
        response,
        prepared_request.request.model.clone(),
        &trace_id,
        tool_registry,
    )
    .await
    {
        Ok(payload) => Ok(json_response_with_bridge(&payload, bridge.as_ref())),
        Err(e) => {
            log::warn!("[{trace_id}] collect error: {e}");
            Ok(anthropic_error(
                StatusCode::BAD_GATEWAY,
                "api_error",
                "Failed to decode upstream response",
            ))
        }
    }
}

async fn handle_bridge_surfaces(state: AppState) -> Result<impl Reply, warp::Rejection> {
    Ok(warp::reply::json(&state.surface_registry.all()))
}

async fn handle_bridge_surface(
    surface_id: String,
    state: AppState,
) -> Result<warp::reply::Response, warp::Rejection> {
    let Some(surface) = state.surface_registry.get(&surface_id) else {
        return Ok(warp::reply::with_status(
            warp::reply::json(&json!({"error":"not_found"})),
            StatusCode::NOT_FOUND,
        )
        .into_response());
    };
    let decision = state
        .compatibility_matrix
        .get(&surface_id, state.operation_mode)
        .cloned();
    Ok(warp::reply::json(&json!({
        "surface": surface,
        "decision": decision,
    }))
    .into_response())
}

async fn handle_bridge_compatibility(state: AppState) -> Result<impl Reply, warp::Rejection> {
    let decisions = state
        .compatibility_matrix
        .all_for_mode(state.operation_mode);
    Ok(warp::reply::json(&decisions))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeJobView {
    id: String,
    session_id: Option<String>,
    thread_id: Option<String>,
    turn_id: Option<String>,
    surface: String,
    kind: String,
    status: String,
    account_id: Option<String>,
    started_at: i64,
    finished_at: Option<i64>,
    duration_secs: Option<i64>,
    result_preview: Option<String>,
    error: Option<String>,
    worktree_path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeSessionView {
    id: String,
    thread_id: String,
    cwd: String,
    turn_count: u64,
    active_jobs: Vec<String>,
    created_at: i64,
}

fn job_status_label(status: &crate::jobs::JobStatus) -> &'static str {
    match status {
        crate::jobs::JobStatus::Queued => "queued",
        crate::jobs::JobStatus::Running => "running",
        crate::jobs::JobStatus::WaitingApproval => "waiting_approval",
        crate::jobs::JobStatus::WaitingClarification => "waiting_clarification",
        crate::jobs::JobStatus::Completed => "completed",
        crate::jobs::JobStatus::Failed => "failed",
        crate::jobs::JobStatus::Cancelled => "cancelled",
    }
}

fn job_kind_label(kind: &crate::jobs::JobKind) -> &'static str {
    match kind {
        crate::jobs::JobKind::Review => "review",
        crate::jobs::JobKind::Rescue => "rescue",
        crate::jobs::JobKind::Task => "task",
        crate::jobs::JobKind::Schedule => "schedule",
        crate::jobs::JobKind::Automation => "automation",
        crate::jobs::JobKind::Subagent => "subagent",
        crate::jobs::JobKind::SessionCron => "session_cron",
        crate::jobs::JobKind::DurableAutomation => "durable_automation",
    }
}

fn preview(text: &Option<String>) -> Option<String> {
    text.as_ref().map(|value| {
        let single_line = value.replace('\n', " ");
        let truncated = single_line.chars().take(180).collect::<String>();
        if single_line.chars().count() > 180 {
            format!("{truncated}…")
        } else {
            single_line
        }
    })
}

async fn handle_bridge_jobs(state: AppState) -> Result<impl Reply, warp::Rejection> {
    let jobs = state
        .job_registry
        .list()
        .await
        .into_iter()
        .map(|job| BridgeJobView {
            duration_secs: job
                .finished_at
                .map(|finished_at| finished_at.saturating_sub(job.created_at)),
            session_id: job.codex_thread_id.clone(),
            thread_id: job.codex_thread_id.clone(),
            turn_id: job.codex_turn_id.clone(),
            surface: job.origin_surface_id.clone(),
            kind: job_kind_label(&job.kind).to_string(),
            status: job_status_label(&job.status).to_string(),
            account_id: job.account_id.clone(),
            started_at: job.created_at,
            finished_at: job.finished_at,
            result_preview: preview(&job.result_summary),
            error: preview(&job.error_message),
            worktree_path: job.worktree_path.clone(),
            id: job.job_id,
        })
        .collect::<Vec<_>>();
    Ok(warp::reply::json(&jobs))
}

async fn handle_bridge_sessions(state: AppState) -> Result<impl Reply, warp::Rejection> {
    let sessions = state
        .state_store
        .list_sessions()
        .await
        .into_iter()
        .map(|session| BridgeSessionView {
            id: session.bridge_session_id,
            thread_id: session.thread.thread_id,
            cwd: session.thread.cwd,
            turn_count: session.thread.turn_count,
            active_jobs: session.active_jobs,
            created_at: session.thread.created_at_unix,
        })
        .collect::<Vec<_>>();
    Ok(warp::reply::json(&sessions))
}

async fn handle_bridge_session(
    session_id: String,
    state: AppState,
) -> Result<warp::reply::Response, warp::Rejection> {
    match state.state_store.get_session(&session_id).await {
        Some(session) => Ok(warp::reply::json(&session).into_response()),
        None => Ok(warp::reply::with_status(
            warp::reply::json(&json!({"error":"not_found"})),
            StatusCode::NOT_FOUND,
        )
        .into_response()),
    }
}

async fn handle_bridge_mode(state: AppState) -> Result<impl Reply, warp::Rejection> {
    let app_server_available = state.app_server_runtime.read().await.is_some();
    let responses_fallback_available =
        response_client_for_auth_path(&state, &state.default_auth_path)
            .await
            .is_some();

    Ok(warp::reply::json(&json!({
        "operationMode": state.operation_mode,
        "apiStability": state.api_stability,
        "delegationPolicy": state.delegation_policy,
        "appServerAvailable": app_server_available,
        "responsesFallbackAvailable": responses_fallback_available,
    })))
}

fn log_surface_decisions(state: &AppState, surfaces: &[ClassifiedSurface]) {
    for surface in surfaces {
        let Some(surface_id) = surface.surface_id.as_deref() else {
            continue;
        };
        let Some(decision) = state
            .compatibility_matrix
            .get(surface_id, state.operation_mode)
        else {
            continue;
        };
        log_mapping_decision(decision);
    }
}

fn bridge_metadata_for_surface_id(state: &AppState, surface_id: &str) -> Option<BridgeMetadata> {
    let surface = state.surface_registry.get(surface_id)?;
    let decision = state
        .compatibility_matrix
        .get(surface_id, state.operation_mode)?;
    Some(BridgeMetadata::from_decision(
        surface,
        decision,
        state.operation_mode,
        state.api_stability,
    ))
}

fn primary_bridge_metadata(
    state: &AppState,
    classified_surfaces: &[ClassifiedSurface],
) -> Option<BridgeMetadata> {
    classified_surfaces
        .iter()
        .filter_map(|surface| surface.surface_id.as_deref())
        .find_map(|surface_id| bridge_metadata_for_surface_id(state, surface_id))
}

fn json_response_with_bridge<T: Serialize>(
    payload: &T,
    bridge: Option<&BridgeMetadata>,
) -> warp::reply::Response {
    let mut value = serde_json::to_value(payload).unwrap_or_else(|_| json!({}));
    if let Some(bridge) = bridge {
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "bridge".to_string(),
                serde_json::to_value(bridge).unwrap_or_else(|_| json!({})),
            );
        }
    }
    warp::reply::json(&value).into_response()
}

#[derive(Debug, Clone)]
struct LocalCommandOutcome {
    surface_id: String,
    body: String,
}

async fn try_anthropic_local_command(
    state: &AppState,
    request: &AnthropicMessagesRequest,
) -> Option<warp::reply::Response> {
    if request.stream.unwrap_or(false) {
        return None;
    }

    let text = latest_anthropic_user_text(request)?;
    let outcome = dispatch_local_command(state, &text).await?;
    let bridge = bridge_metadata_for_surface_id(state, &outcome.surface_id);
    let payload = AnthropicMessagesResponse {
        id: format!("msg_{}", Uuid::new_v4()),
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        model: request.model.clone(),
        content: vec![AnthropicResponseContentBlock::Text { text: outcome.body }],
        stop_reason: "end_turn".to_string(),
        stop_sequence: None,
        usage: AnthropicUsage {
            input_tokens: 0,
            output_tokens: 0,
        },
    };
    Some(json_response_with_bridge(&payload, bridge.as_ref()))
}

async fn try_openai_local_command(
    state: &AppState,
    request: &ChatCompletionsRequest,
) -> Option<warp::reply::Response> {
    if request.stream.unwrap_or(false) {
        return None;
    }

    let text = latest_openai_user_text(request)?;
    let outcome = dispatch_local_command(state, &text).await?;
    let bridge = bridge_metadata_for_surface_id(state, &outcome.surface_id);
    let payload = ChatCompletionsResponse {
        id: format!("chatcmpl-{}", Uuid::new_v4()),
        object: "chat.completion".to_string(),
        created: chrono::Utc::now().timestamp(),
        model: request.model.clone(),
        choices: vec![OpenAIChoice {
            index: 0,
            message: OpenAIResponseMessage {
                role: "assistant".to_string(),
                content: Some(outcome.body),
                tool_calls: None,
            },
            finish_reason: "stop".to_string(),
        }],
        usage: OpenAIUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        },
    };
    Some(json_response_with_bridge(&payload, bridge.as_ref()))
}

async fn dispatch_local_command(state: &AppState, text: &str) -> Option<LocalCommandOutcome> {
    let (command, args) = parse_command_line(text)?;
    let cwd = std::env::current_dir()
        .ok()
        .map(|dir| dir.display().to_string())
        .unwrap_or_else(|| ".".to_string());

    match command {
        "/tasks" => Some(LocalCommandOutcome {
            surface_id: "command.tasks".to_string(),
            body: render_command_result(&map_tasks_command(&state.job_registry).await),
        }),
        "/security-review" => {
            let request = ReviewRequest {
                scope: if args.is_empty() {
                    None
                } else {
                    Some(args.to_string())
                },
                files: None,
                instructions: None,
            };
            let executor = local_command_executor(state).await;
            Some(LocalCommandOutcome {
                surface_id: "command.security_review".to_string(),
                body: render_command_result(
                    &map_security_review_command(request, executor.as_deref(), &state.job_registry)
                        .await,
                ),
            })
        }
        "/plan" => {
            let result = map_plan_command((!args.is_empty()).then_some(args));
            Some(LocalCommandOutcome {
                surface_id: "command.plan".to_string(),
                body: render_plan_result(&result, args),
            })
        }
        "/schedule" => Some(LocalCommandOutcome {
            surface_id: "command.schedule".to_string(),
            body: render_command_result(&map_schedule_command()),
        }),
        "/init" => Some(LocalCommandOutcome {
            surface_id: "command.init".to_string(),
            body: render_guidance_init(&map_init_guidance(&cwd)),
        }),
        "/memory" => Some(LocalCommandOutcome {
            surface_id: "command.memory".to_string(),
            body: render_memory_import(&map_memory_import(&cwd)),
        }),
        "/mcp" => Some(LocalCommandOutcome {
            surface_id: "command.mcp".to_string(),
            body: render_command_result(&map_mcp_command(if args.is_empty() {
                "status"
            } else {
                args
            })),
        }),
        "/plugin" => Some(LocalCommandOutcome {
            surface_id: "command.plugin".to_string(),
            body: render_command_result(&map_plugin_command(if args.is_empty() {
                "list"
            } else {
                args
            })),
        }),
        _ => None,
    }
}

async fn local_command_executor(state: &AppState) -> Option<Arc<JobExecutor>> {
    let auth_path = select_request_auth_path(state).await?;
    match ensure_app_server_runtime(state, &auth_path).await {
        Ok(Some(runtime)) => Some(runtime.executor),
        _ => None,
    }
}

fn latest_anthropic_user_text(request: &AnthropicMessagesRequest) -> Option<String> {
    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .and_then(|message| flatten_anthropic_content(&message.content))
}

fn latest_anthropic_user_turn_for_app_server(request: &AnthropicMessagesRequest) -> Option<String> {
    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .and_then(flatten_anthropic_message_for_app_server)
}

fn latest_openai_user_text(request: &ChatCompletionsRequest) -> Option<String> {
    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .and_then(|message| message.content.as_ref())
        .and_then(flatten_openai_content)
}

fn latest_openai_user_turn_for_app_server(request: &ChatCompletionsRequest) -> Option<String> {
    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .and_then(flatten_openai_message_for_app_server)
}

fn parse_command_line(text: &str) -> Option<(&str, &str)> {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return None;
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let command = parts.next()?;
    let args = parts.next().map(str::trim).unwrap_or("");
    Some((command, args))
}

fn render_command_result(result: &CommandResult) -> String {
    let mut body = result.message.clone();
    if !result.data.is_null() && result.data != json!({}) && result.data != json!([]) {
        let rendered = serde_json::to_string_pretty(&result.data).unwrap_or_default();
        if !rendered.is_empty() {
            body.push_str("\n\n");
            body.push_str(&rendered);
        }
    }
    body
}

fn render_plan_result(result: &crate::mapping::planning::PlanModeResult, args: &str) -> String {
    let mut body = if args.is_empty() {
        "Entered plan mode with the default planning profile.".to_string()
    } else {
        format!("Entered plan mode with plan instructions:\n\n{}", args)
    };
    if !result.warnings.is_empty() {
        body.push_str("\n\nWarnings:");
        for warning in &result.warnings {
            body.push_str("\n- ");
            body.push_str(&warning.warning);
        }
    }
    body
}

fn render_guidance_init(result: &crate::mapping::guidance::GuidanceInitResult) -> String {
    format!("Proposed guidance bootstrap at `{}`.", result.proposed_path)
}

fn render_memory_import(result: &crate::mapping::guidance::MemoryImportResult) -> String {
    let mut body = format!(
        "Memory import proposal prepared from `{}` to `{}`.",
        result.source_path.as_deref().unwrap_or("CLAUDE.md"),
        result.target_path
    );
    if !result.warnings.is_empty() {
        body.push_str("\n\nWarnings:");
        for warning in &result.warnings {
            body.push_str("\n- ");
            body.push_str(warning);
        }
    }
    body
}

async fn request_with_tool_fallback(
    client: &CodexClient,
    mut request: CodexResponsesRequest,
    has_tools: bool,
    trace_id: &str,
) -> std::result::Result<reqwest::Response, UpstreamError> {
    let disable_fallback = tool_fallback_disabled();
    let tool_required = tool_choice_requires_tool(request.tool_choice.as_ref());

    match client.create_response(&request).await {
        Ok(v) => Ok(v),
        Err(UpstreamError::Upstream { status, body })
            if has_tools && CodexClient::is_tool_unsupported(&body) =>
        {
            if disable_fallback || tool_required {
                log::warn!(
                    "[{trace_id}] tool unsupported by upstream; fallback disabled (required={tool_required}, env_disabled={disable_fallback})"
                );
                return Err(UpstreamError::Upstream { status, body });
            }

            log::warn!("[{trace_id}] tool unsupported by upstream, retrying once without tools");
            request.tools = None;
            request.tool_choice = None;
            client.create_response(&request).await
        }
        Err(e) => Err(e),
    }
}

fn openai_error(status: StatusCode, message: &str, error_type: &str) -> warp::reply::Response {
    warp::reply::with_status(
        warp::reply::json(&json!({
            "error": {
                "message": message,
                "type": error_type,
                "code": error_type
            }
        })),
        status,
    )
    .into_response()
}

fn anthropic_error(status: StatusCode, error_type: &str, message: &str) -> warp::reply::Response {
    let body = AnthropicErrorBody {
        body_type: "error".to_string(),
        error: AnthropicError {
            error_type: error_type.to_string(),
            message: message.to_string(),
        },
    };

    warp::reply::with_status(warp::reply::json(&body), status).into_response()
}

fn anthropic_error_type_for_status(status: StatusCode) -> &'static str {
    if status == StatusCode::TOO_MANY_REQUESTS {
        return "rate_limit_error";
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return "authentication_error";
    }
    if status.is_client_error() {
        return "invalid_request_error";
    }
    "api_error"
}

fn anthropic_error_message_for_status(status: StatusCode) -> &'static str {
    if status == StatusCode::TOO_MANY_REQUESTS {
        return "Rate limit reached on upstream provider";
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return "Authentication failed on upstream provider";
    }
    if status.is_client_error() {
        return "Upstream rejected request";
    }
    "Upstream error"
}

fn log_request_summary(
    trace_id: &str,
    protocol: &str,
    model: &str,
    message_count: usize,
    stream: bool,
    tool_count: usize,
    headers: &warp::http::HeaderMap,
) {
    let auth_mode = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|_| "auth_header")
        .unwrap_or("no_auth_header");

    log::info!(
        "[{trace_id}] protocol={protocol} model={model} stream={stream} messages={message_count} tools={tool_count} auth={auth_mode}"
    );
}

fn log_surface_summary(trace_id: &str, surfaces: &[crate::surfaces::ClassifiedSurface]) {
    if surfaces.is_empty() {
        return;
    }
    let names = surfaces
        .iter()
        .map(|surface| surface.requested_name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    log::info!("[{trace_id}] classified_surfaces={names}");
}

fn truncate(v: &str, max: usize) -> String {
    if v.len() <= max {
        return v.to_string();
    }

    format!("{}...", &v[..max])
}

fn is_recoverable_continuation_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "usage limit",
        "quota",
        "rate limit",
        "could not be refreshed",
        "refresh token was already used",
        "log out and sign in again",
        "thread not found",
        "job missing thread id",
    ]
    .iter()
    .any(|signal| lower.contains(signal))
}

fn tool_choice_requires_tool(choice: Option<&CodexToolChoice>) -> bool {
    match choice {
        Some(CodexToolChoice::Function { .. }) => true,
        Some(CodexToolChoice::Strategy(v)) => {
            let v = v.to_ascii_lowercase();
            v == "required" || v == "any" || v == "tool"
        }
        None => false,
    }
}

fn tool_fallback_disabled() -> bool {
    std::env::var("DISABLE_TOOL_FALLBACK")
        .ok()
        .map(|v| {
            let v = v.to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes"
        })
        .unwrap_or(false)
}

/// Returns the cached 429 body if the upstream rate-limit is still active.
async fn check_rate_limit_guard(state: &AppState) -> Option<serde_json::Value> {
    let guard = state.rate_limit_guard.read().await;
    if let Some(cached) = guard.as_ref() {
        if std::time::Instant::now() < cached.expires_at {
            return Some(cached.body.clone());
        }
    }
    None
}

/// Parses a 429 body for `resets_in_seconds` and caches it so subsequent
/// retries from the client are rejected instantly without hitting upstream.
/// Also reports to the per-account pool for rate-limit tracking.
async fn cache_rate_limit(state: &AppState, body: &str, auth_path: Option<&Path>) {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return,
    };

    let resets_in = parsed
        .get("error")
        .and_then(|e| e.get("resets_in_seconds"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if resets_in == 0 {
        return;
    }

    // Global guard (IP-level safety net)
    let ttl = Duration::from_secs(resets_in.min(MAX_RATE_LIMIT_TTL_SECS));
    let mut guard = state.rate_limit_guard.write().await;
    *guard = Some(CachedRateLimit {
        body: parsed,
        expires_at: std::time::Instant::now() + ttl,
    });
    drop(guard);

    if let Some(auth_path) = auth_path {
        state
            .pool
            .report_rate_limit(&auth_path.to_path_buf(), resets_in)
            .await;
    }
}

fn app_server_turn_timeout() -> Duration {
    std::env::var("APP_SERVER_TURN_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(300))
}

fn primary_surface_id(surfaces: &[ClassifiedSurface]) -> Option<String> {
    surfaces
        .iter()
        .find_map(|surface| surface.surface_id.clone())
}

fn job_kind_for_surfaces(surfaces: &[ClassifiedSurface]) -> JobKind {
    for surface in surfaces {
        match surface.surface_id.as_deref() {
            Some("workflow.rescue_fix") => return JobKind::Rescue,
            Some("workflow.code_review")
            | Some("workflow.security_review")
            | Some("command.security_review") => return JobKind::Review,
            Some("tool.agent") | Some("tool.sendmessage") => return JobKind::Subagent,
            Some("tool.taskcreate")
            | Some("tool.taskget")
            | Some("tool.tasklist")
            | Some("tool.taskupdate")
            | Some("tool.taskstop")
            | Some("command.tasks")
            | Some("command.resume") => return JobKind::Task,
            _ => {}
        }
    }
    JobKind::Task
}

fn build_anthropic_app_server_prompt(
    request: &AnthropicMessagesRequest,
) -> Option<(Option<String>, String)> {
    let system_prompt = request.system.as_ref().and_then(flatten_anthropic_system);
    let mut messages = Vec::new();

    for message in &request.messages {
        let content = flatten_anthropic_message_for_app_server(message)?;
        messages.push(format!("{}: {}", message.role, content));
    }

    Some((system_prompt, messages.join("\n\n")))
}

fn build_openai_app_server_prompt(request: &ChatCompletionsRequest) -> Option<String> {
    let mut messages = Vec::new();
    for message in &request.messages {
        let content = flatten_openai_message_for_app_server(message)?;
        messages.push(format!("{}: {}", message.role, content));
    }

    Some(messages.join("\n\n"))
}

fn make_background_ack(model: &str, body: &str) -> AnthropicMessagesResponse {
    AnthropicMessagesResponse {
        id: format!("msg_{}", Uuid::new_v4().simple()),
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        model: model.to_string(),
        content: vec![AnthropicResponseContentBlock::Text {
            text: body.to_string(),
        }],
        stop_reason: "end_turn".to_string(),
        stop_sequence: None,
        usage: AnthropicUsage {
            input_tokens: 0,
            output_tokens: 0,
        },
    }
}

fn make_openai_background_ack(model: &str, body: &str) -> ChatCompletionsResponse {
    ChatCompletionsResponse {
        id: format!("chatcmpl-{}", Uuid::new_v4()),
        object: "chat.completion".to_string(),
        created: chrono::Utc::now().timestamp(),
        model: model.to_string(),
        choices: vec![OpenAIChoice {
            index: 0,
            message: OpenAIResponseMessage {
                role: "assistant".to_string(),
                content: Some(body.to_string()),
                tool_calls: None,
            },
            finish_reason: "stop".to_string(),
        }],
        usage: OpenAIUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        },
    }
}

fn flatten_anthropic_system(system: &crate::domain::anthropic::AnthropicSystem) -> Option<String> {
    match system {
        crate::domain::anthropic::AnthropicSystem::Text(text) => Some(text.clone()),
        crate::domain::anthropic::AnthropicSystem::Blocks(blocks) => Some(
            blocks
                .iter()
                .filter_map(|block| block.text.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    }
}

fn flatten_anthropic_content(
    content: &crate::domain::anthropic::AnthropicContent,
) -> Option<String> {
    match content {
        crate::domain::anthropic::AnthropicContent::Text(text) => Some(text.clone()),
        crate::domain::anthropic::AnthropicContent::Blocks(blocks) => {
            let mut out = Vec::new();
            for block in blocks {
                match block {
                    crate::domain::anthropic::AnthropicContentBlock::Text { text } => {
                        out.push(text.clone())
                    }
                    _ => return None,
                }
            }
            Some(out.join("\n"))
        }
    }
}

fn flatten_anthropic_message_for_app_server(
    message: &crate::domain::anthropic::AnthropicMessage,
) -> Option<String> {
    match &message.content {
        crate::domain::anthropic::AnthropicContent::Text(text) => Some(text.clone()),
        crate::domain::anthropic::AnthropicContent::Blocks(blocks) => {
            let mut out = Vec::new();
            for block in blocks {
                match block {
                    crate::domain::anthropic::AnthropicContentBlock::Text { text } => {
                        out.push(text.clone())
                    }
                    crate::domain::anthropic::AnthropicContentBlock::ToolUse {
                        id,
                        name,
                        input,
                    } => {
                        out.push(format!(
                            "[tool_use id={} name={} input={}]",
                            id,
                            name,
                            serde_json::to_string(input).ok()?
                        ));
                    }
                    crate::domain::anthropic::AnthropicContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        out.push(format!(
                            "[tool_result id={} is_error={} content={}]",
                            tool_use_id,
                            is_error.unwrap_or(false),
                            render_jsonish_value(content)
                        ));
                    }
                    crate::domain::anthropic::AnthropicContentBlock::Unknown => return None,
                    crate::domain::anthropic::AnthropicContentBlock::Image { .. } => return None,
                }
            }
            Some(out.join("\n"))
        }
    }
}

fn flatten_openai_content(content: &crate::domain::openai::OpenAIContent) -> Option<String> {
    match content {
        crate::domain::openai::OpenAIContent::Text(text) => Some(text.clone()),
        crate::domain::openai::OpenAIContent::Parts(parts) => {
            let mut out = Vec::new();
            for part in parts {
                match part {
                    crate::domain::openai::OpenAIContentPart::Text { text } => {
                        out.push(text.clone().unwrap_or_default())
                    }
                    _ => return None,
                }
            }
            Some(out.join("\n"))
        }
    }
}

fn flatten_openai_message_for_app_server(
    message: &crate::domain::openai::OpenAIMessage,
) -> Option<String> {
    let mut parts = Vec::new();

    if let Some(content) = message.content.as_ref() {
        parts.push(flatten_openai_content(content)?);
    }

    if let Some(tool_calls) = message.tool_calls.as_ref() {
        for call in tool_calls {
            parts.push(format!(
                "[tool_call id={} name={} arguments={}]",
                call.id, call.function.name, call.function.arguments
            ));
        }
    }

    if let Some(function_call) = message.function_call.as_ref() {
        parts.push(format!(
            "[function_call name={} arguments={}]",
            function_call.name, function_call.arguments
        ));
    }

    if message.role == "tool" {
        parts.push(format!(
            "[tool_result id={} content={}]",
            message.tool_call_id.as_deref().unwrap_or("unknown"),
            message
                .content
                .as_ref()
                .and_then(flatten_openai_content)
                .unwrap_or_default()
        ));
    }

    if parts.is_empty() {
        return None;
    }

    Some(parts.join("\n"))
}

fn render_jsonish_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    }
}

#[cfg(test)]
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
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::accounts::AccountPool;
    use crate::domain::anthropic::{AnthropicContent, AnthropicMessage, AnthropicSystem};
    use crate::skills::load_skill_registry;
    use crate::surfaces::CompatibilityMatrix;
    use crate::surfaces::SurfaceRegistry;

    fn test_pool_from_config() -> Arc<AccountPool> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("claude-codex-proxy-routes-{unique}"));
        fs::create_dir_all(&directory).expect("create temp directory");
        let config_path = directory.join("accounts.toml");
        let config = r#"
[pool]
degrade_threshold = 3
cooldown_seconds = 60

[[account]]
id = "account_1"
label = "Account 1"
auth_path = "/tmp/account-1/auth.json"
enabled = true

[[account]]
id = "account_2"
label = "Account 2"
auth_path = "/tmp/account-2/auth.json"
enabled = true
"#;
        fs::write(&config_path, config).expect("write accounts config");
        AccountPool::from_config_file(config_path.to_str().expect("utf8 path")).expect("pool")
    }

    fn test_state() -> AppState {
        let registry = SurfaceRegistry::new();
        let matrix = CompatibilityMatrix::new(&registry);
        AppState {
            default_auth_path: PathBuf::from("/tmp/default/auth.json"),
            response_clients: Arc::new(Mutex::new(HashMap::new())),
            app_server_runtime: Arc::new(RwLock::new(None)),
            skill_registry: None,
            surface_registry: Arc::new(registry.clone()),
            compatibility_matrix: Arc::new(matrix),
            classifier: Arc::new(SurfaceClassifier::new(registry)),
            job_registry: JobRegistry::default(),
            state_store: StateStore::default(),
            operation_mode: OperationMode::AutoHybrid,
            api_stability: ApiStability::Stable,
            delegation_policy: DelegationPolicy::ExplicitOnly,
            rate_limit_guard: Arc::new(RwLock::new(None)),
            pool: test_pool_from_config(),
            account_sync_clock: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn detects_required_tool_choice() {
        assert!(tool_choice_requires_tool(Some(&CodexToolChoice::Strategy(
            "required".to_string(),
        ))));
        assert!(tool_choice_requires_tool(Some(
            &CodexToolChoice::Function {
                choice_type: "function".to_string(),
                name: "read_file".to_string(),
            }
        )));
        assert!(!tool_choice_requires_tool(Some(
            &CodexToolChoice::Strategy("auto".to_string(),)
        )));
    }

    #[test]
    fn prepares_anthropic_request_with_skill_bridge_fixture() {
        let registry = load_skill_registry(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/skill_bridge/registry.json"),
        )
        .expect("registry");
        let request = AnthropicMessagesRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("review these changes".to_string()),
            }],
            system: Some(AnthropicSystem::Text(
                "skill-bridge:code-review@1.0.0".to_string(),
            )),
            tools: None,
            tool_choice: None,
            stream: Some(false),
            thinking: None,
        };

        let prepared =
            prepare_anthropic_request(&request, Some(&registry), "test-resolve-anthropic");

        assert!(prepared.bridge.is_some());
        assert!(prepared.request.system.is_none());
    }

    #[test]
    fn parses_command_line_with_arguments() {
        let (command, args) = parse_command_line("/plan ship the patch").expect("command");
        assert_eq!(command, "/plan");
        assert_eq!(args, "ship the patch");
    }

    #[test]
    fn parses_structured_sandbox_policy() {
        let sandbox = parse_sandbox_config(&serde_json::json!({ "type": "dangerFullAccess" }));
        assert_eq!(sandbox, SandboxConfig::DangerFullAccess);
    }

    #[tokio::test]
    async fn local_tasks_command_dispatches() {
        let registry = SurfaceRegistry::new();
        let matrix = CompatibilityMatrix::new(&registry);
        let state = AppState {
            default_auth_path: PathBuf::from("/tmp/default/auth.json"),
            response_clients: Arc::new(Mutex::new(HashMap::new())),
            app_server_runtime: Arc::new(RwLock::new(None)),
            skill_registry: None,
            surface_registry: Arc::new(registry.clone()),
            compatibility_matrix: Arc::new(matrix),
            classifier: Arc::new(SurfaceClassifier::new(registry)),
            job_registry: JobRegistry::default(),
            state_store: StateStore::default(),
            operation_mode: OperationMode::AutoHybrid,
            api_stability: ApiStability::Stable,
            delegation_policy: DelegationPolicy::ExplicitOnly,
            rate_limit_guard: Arc::new(RwLock::new(None)),
            pool: crate::accounts::load_pool(),
            account_sync_clock: Arc::new(Mutex::new(None)),
        };

        let response = dispatch_local_command(&state, "/tasks")
            .await
            .expect("command");
        assert_eq!(response.surface_id, "command.tasks");
        assert!(response.body.contains("0 active jobs"));
    }

    #[tokio::test]
    async fn local_security_review_command_creates_job() {
        let registry = SurfaceRegistry::new();
        let matrix = CompatibilityMatrix::new(&registry);
        let state = AppState {
            default_auth_path: PathBuf::from("/tmp/default/auth.json"),
            response_clients: Arc::new(Mutex::new(HashMap::new())),
            app_server_runtime: Arc::new(RwLock::new(None)),
            skill_registry: None,
            surface_registry: Arc::new(registry.clone()),
            compatibility_matrix: Arc::new(matrix),
            classifier: Arc::new(SurfaceClassifier::new(registry)),
            job_registry: JobRegistry::default(),
            state_store: StateStore::default(),
            operation_mode: OperationMode::AutoHybrid,
            api_stability: ApiStability::Stable,
            delegation_policy: DelegationPolicy::ExplicitOnly,
            rate_limit_guard: Arc::new(RwLock::new(None)),
            pool: crate::accounts::load_pool(),
            account_sync_clock: Arc::new(Mutex::new(None)),
        };

        let response = dispatch_local_command(&state, "/security-review src/")
            .await
            .expect("command");
        assert_eq!(response.surface_id, "command.security_review");
        assert!(response.body.contains("Security review started"));
        assert_eq!(state.job_registry.list().await.len(), 1);
    }

    #[tokio::test]
    async fn select_request_auth_path_falls_back_when_default_account_disabled() {
        let registry = SurfaceRegistry::new();
        let matrix = CompatibilityMatrix::new(&registry);
        let pool = test_pool_from_config();
        pool.toggle("account_1").await.expect("toggle account");
        let state = AppState {
            default_auth_path: PathBuf::from("/tmp/account-1/auth.json"),
            response_clients: Arc::new(Mutex::new(HashMap::new())),
            app_server_runtime: Arc::new(RwLock::new(None)),
            skill_registry: None,
            surface_registry: Arc::new(registry.clone()),
            compatibility_matrix: Arc::new(matrix),
            classifier: Arc::new(SurfaceClassifier::new(registry)),
            job_registry: JobRegistry::default(),
            state_store: StateStore::default(),
            operation_mode: OperationMode::AutoHybrid,
            api_stability: ApiStability::Stable,
            delegation_policy: DelegationPolicy::ExplicitOnly,
            rate_limit_guard: Arc::new(RwLock::new(None)),
            pool,
            account_sync_clock: Arc::new(Mutex::new(None)),
        };

        let selected = select_request_auth_path(&state)
            .await
            .expect("selected path");
        assert_eq!(selected, PathBuf::from("/tmp/account-2/auth.json"));
    }

    #[tokio::test]
    async fn continuation_session_prefers_client_session_header() {
        let state = test_state();
        state
            .state_store
            .insert_session(crate::app_server::BridgeSession {
                bridge_session_id: "bridge-1".to_string(),
                claude_session_id: Some("client-session-1".to_string()),
                account_id: Some("account_1".to_string()),
                account_auth_path: Some("/tmp/account-1/auth.json".to_string()),
                last_assistant_message: Some("ready".to_string()),
                thread: crate::app_server::BridgeThread {
                    thread_id: "thread-1".to_string(),
                    bridge_session_id: "bridge-1".to_string(),
                    cwd: "/tmp/project".to_string(),
                    project_root: None,
                    approval_policy: crate::mapping::approvals::ApprovalPolicy::OnRequest,
                    sandbox_config: crate::mapping::approvals::SandboxConfig::WorkspaceWrite,
                    created_at_unix: 1,
                    turn_count: 1,
                },
                transport: crate::app_server::TransportKind::Stdio,
                operation_mode: OperationMode::AutoHybrid,
                api_stability: ApiStability::Stable,
                delegation_policy: DelegationPolicy::ExplicitOnly,
                active_guidance_layers: Vec::new(),
                active_skills: Vec::new(),
                active_jobs: Vec::new(),
                state_version: 1,
            })
            .await;

        let mut headers = warp::http::HeaderMap::new();
        headers.insert(
            "x-claude-session-id",
            warp::http::HeaderValue::from_static("client-session-1"),
        );
        let request = ChatCompletionsRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![crate::domain::openai::OpenAIMessage {
                role: "user".to_string(),
                content: Some(crate::domain::openai::OpenAIContent::Text(
                    "continue".to_string(),
                )),
                tool_calls: None,
                function_call: None,
                tool_call_id: None,
                name: None,
            }],
            stream: Some(false),
            tools: None,
            tool_choice: None,
            functions: None,
            reasoning_effort: None,
            response_format: None,
        };

        let session = resolve_openai_continuation_session(&state, &headers, &request)
            .await
            .expect("continuation session");
        assert_eq!(session.thread.thread_id, "thread-1");
        assert_eq!(
            session.account_auth_path.as_deref(),
            Some("/tmp/account-1/auth.json")
        );
    }

    #[tokio::test]
    async fn continuation_session_header_prefers_newest_session() {
        let state = test_state();
        state
            .state_store
            .insert_session(crate::app_server::BridgeSession {
                bridge_session_id: "bridge-old".to_string(),
                claude_session_id: Some("client-session-1".to_string()),
                account_id: Some("account_legacy".to_string()),
                account_auth_path: None,
                last_assistant_message: Some("old".to_string()),
                thread: crate::app_server::BridgeThread {
                    thread_id: "thread-old".to_string(),
                    bridge_session_id: "bridge-old".to_string(),
                    cwd: "/tmp/project".to_string(),
                    project_root: None,
                    approval_policy: crate::mapping::approvals::ApprovalPolicy::OnRequest,
                    sandbox_config: crate::mapping::approvals::SandboxConfig::WorkspaceWrite,
                    created_at_unix: 1,
                    turn_count: 1,
                },
                transport: crate::app_server::TransportKind::Stdio,
                operation_mode: OperationMode::AutoHybrid,
                api_stability: ApiStability::Stable,
                delegation_policy: DelegationPolicy::ExplicitOnly,
                active_guidance_layers: Vec::new(),
                active_skills: Vec::new(),
                active_jobs: Vec::new(),
                state_version: 1,
            })
            .await;
        state
            .state_store
            .insert_session(crate::app_server::BridgeSession {
                bridge_session_id: "bridge-new".to_string(),
                claude_session_id: Some("client-session-1".to_string()),
                account_id: Some("account_1".to_string()),
                account_auth_path: Some("/tmp/account-1/auth.json".to_string()),
                last_assistant_message: Some("new".to_string()),
                thread: crate::app_server::BridgeThread {
                    thread_id: "thread-new".to_string(),
                    bridge_session_id: "bridge-new".to_string(),
                    cwd: "/tmp/project".to_string(),
                    project_root: None,
                    approval_policy: crate::mapping::approvals::ApprovalPolicy::OnRequest,
                    sandbox_config: crate::mapping::approvals::SandboxConfig::WorkspaceWrite,
                    created_at_unix: 2,
                    turn_count: 4,
                },
                transport: crate::app_server::TransportKind::Stdio,
                operation_mode: OperationMode::AutoHybrid,
                api_stability: ApiStability::Stable,
                delegation_policy: DelegationPolicy::ExplicitOnly,
                active_guidance_layers: Vec::new(),
                active_skills: Vec::new(),
                active_jobs: Vec::new(),
                state_version: 1,
            })
            .await;

        let mut headers = warp::http::HeaderMap::new();
        headers.insert(
            "x-claude-session-id",
            warp::http::HeaderValue::from_static("client-session-1"),
        );
        let request = ChatCompletionsRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![crate::domain::openai::OpenAIMessage {
                role: "user".to_string(),
                content: Some(crate::domain::openai::OpenAIContent::Text(
                    "continue".to_string(),
                )),
                tool_calls: None,
                function_call: None,
                tool_call_id: None,
                name: None,
            }],
            stream: Some(false),
            tools: None,
            tool_choice: None,
            functions: None,
            reasoning_effort: None,
            response_format: None,
        };

        let session = resolve_openai_continuation_session(&state, &headers, &request)
            .await
            .expect("continuation session");
        assert_eq!(session.thread.thread_id, "thread-new");
        assert_eq!(
            session.account_auth_path.as_deref(),
            Some("/tmp/account-1/auth.json")
        );
    }

    #[tokio::test]
    async fn continuation_session_falls_back_to_last_assistant_message_affinity() {
        let state = test_state();
        state
            .state_store
            .insert_session(crate::app_server::BridgeSession {
                bridge_session_id: "bridge-2".to_string(),
                claude_session_id: None,
                account_id: Some("account_2".to_string()),
                account_auth_path: Some("/tmp/account-2/auth.json".to_string()),
                last_assistant_message: Some(
                    "Thread is live. Send the task you want handled here.".to_string(),
                ),
                thread: crate::app_server::BridgeThread {
                    thread_id: "thread-2".to_string(),
                    bridge_session_id: "bridge-2".to_string(),
                    cwd: "/tmp/project".to_string(),
                    project_root: None,
                    approval_policy: crate::mapping::approvals::ApprovalPolicy::OnRequest,
                    sandbox_config: crate::mapping::approvals::SandboxConfig::WorkspaceWrite,
                    created_at_unix: 2,
                    turn_count: 2,
                },
                transport: crate::app_server::TransportKind::Stdio,
                operation_mode: OperationMode::AutoHybrid,
                api_stability: ApiStability::Stable,
                delegation_policy: DelegationPolicy::ExplicitOnly,
                active_guidance_layers: Vec::new(),
                active_skills: Vec::new(),
                active_jobs: Vec::new(),
                state_version: 1,
            })
            .await;

        let request = AnthropicMessagesRequest {
            model: "gpt-5.4".to_string(),
            messages: vec![
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: AnthropicContent::Text(
                        "Thread is live. Send the task you want handled here.".to_string(),
                    ),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Text("continue".to_string()),
                },
            ],
            system: None,
            tools: None,
            tool_choice: None,
            stream: Some(false),
            thinking: None,
        };

        let session =
            resolve_anthropic_continuation_session(&state, &warp::http::HeaderMap::new(), &request)
                .await
                .expect("continuation session");
        assert_eq!(session.thread.thread_id, "thread-2");
        assert_eq!(
            latest_anthropic_user_turn_for_app_server(&request).as_deref(),
            Some("continue")
        );
    }

    #[tokio::test]
    async fn bridge_jobs_exposes_dashboard_friendly_fields() {
        let registry = SurfaceRegistry::new();
        let matrix = CompatibilityMatrix::new(&registry);
        let job_registry = JobRegistry::default();
        job_registry
            .insert(crate::jobs::JobRecord {
                job_id: "job-1".to_string(),
                origin_surface_id: "tool.task_create".to_string(),
                kind: crate::jobs::JobKind::Task,
                status: crate::jobs::JobStatus::Failed,
                scheduler_mode: None,
                codex_thread_id: Some("thread-1".to_string()),
                codex_turn_id: Some("turn-1".to_string()),
                codex_agent_ids: Vec::new(),
                worktree_path: None,
                account_id: Some("account_2".to_string()),
                account_auth_path: Some("/tmp/account-2/auth.json".to_string()),
                created_at: 100,
                finished_at: Some(112),
                result_summary: Some("hello\nworld".to_string()),
                warnings: Vec::new(),
                error_message: Some("quota exceeded".to_string()),
            })
            .await;
        let state = AppState {
            default_auth_path: PathBuf::from("/tmp/default/auth.json"),
            response_clients: Arc::new(Mutex::new(HashMap::new())),
            app_server_runtime: Arc::new(RwLock::new(None)),
            skill_registry: None,
            surface_registry: Arc::new(registry.clone()),
            compatibility_matrix: Arc::new(matrix),
            classifier: Arc::new(SurfaceClassifier::new(registry)),
            job_registry,
            state_store: StateStore::default(),
            operation_mode: OperationMode::AutoHybrid,
            api_stability: ApiStability::Stable,
            delegation_policy: DelegationPolicy::ExplicitOnly,
            rate_limit_guard: Arc::new(RwLock::new(None)),
            pool: test_pool_from_config(),
            account_sync_clock: Arc::new(Mutex::new(None)),
        };

        let response = handle_bridge_jobs(state).await.unwrap().into_response();
        let body = warp::hyper::body::to_bytes(response.into_body())
            .await
            .unwrap();
        let payload = String::from_utf8(body.to_vec()).unwrap();
        assert!(payload.contains("\"accountId\":\"account_2\""));
        assert!(payload.contains("\"sessionId\":\"thread-1\""));
        assert!(payload.contains("\"durationSecs\":12"));
        assert!(payload.contains("\"status\":\"failed\""));
    }
}
