use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde::Serialize;
use serde_json::json;
use tokio::sync::{broadcast, RwLock};
use tokio::time::timeout;
use uuid::Uuid;
use warp::{http::StatusCode, Filter, Reply};

use crate::adapters::claude_output::BridgeMetadata;
use crate::app_server::{
    ApiStability, AppServerClient, ThreadStartRequest, TurnStartRequest, UserInput,
};
use crate::app_server::{BridgeSession, BridgeThread, DelegationPolicy, TransportKind};
use crate::domain::anthropic::{AnthropicError, AnthropicErrorBody, AnthropicMessagesRequest};
use crate::domain::anthropic::{
    AnthropicMessagesResponse, AnthropicResponseContentBlock, AnthropicUsage,
};
use crate::domain::codex::{CodexResponsesRequest, CodexToolChoice};
use crate::domain::openai::{
    ChatCompletionsRequest, ChatCompletionsResponse, OpenAIChoice, OpenAIResponseMessage,
    OpenAIUsage,
};
use crate::jobs::JobRegistry;
use crate::mapping::approvals::{ApprovalPolicy, SandboxConfig};
use crate::mapping::commands::{
    map_mcp_command, map_plugin_command, map_schedule_command, map_security_review_command,
    map_tasks_command, CommandResult,
};
use crate::mapping::guidance::{map_init_guidance, map_memory_import};
use crate::mapping::planning::map_plan_command;
use crate::mapping::review::ReviewRequest;
use crate::model_profiles::{expand_public_models, resolve_model_profile};
use crate::observability::traces::log_mapping_decision;
use crate::proxy::codex_client::{CodexClient, UpstreamError};
use crate::skills::{prepare_anthropic_request, SkillRegistry};
use crate::state::StateStore;
use crate::surfaces::{
    ClassifiedSurface, CompatibilityMatrix, OperationMode, SurfaceClassifier, SurfaceRegistry,
};
use crate::translation::anthropic_to_codex::translate_anthropic_to_codex;
use crate::translation::anthropic_to_codex::effective_anthropic_reasoning_effort;
use crate::translation::codex_to_anthropic::{
    collect_codex_to_anthropic, stream_codex_to_anthropic,
};
use crate::translation::codex_to_openai::{collect_codex_to_openai, stream_codex_to_openai};
use crate::translation::openai_to_codex::translate_openai_to_codex;
use crate::translation::openai_to_codex::effective_openai_reasoning_effort;
use crate::translation::tool_runtime::ToolRegistry;

/// Cached upstream rate-limit so subsequent retries are rejected instantly.
#[derive(Clone)]
struct CachedRateLimit {
    /// The raw JSON body from the 429 response — forwarded as-is to clients.
    body: serde_json::Value,
    /// Instant after which this cache entry is considered stale.
    expires_at: std::time::Instant,
}

#[derive(Clone)]
struct AppState {
    client: Option<Arc<CodexClient>>,
    app_server: Option<Arc<AppServerClient>>,
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
}

pub struct RouteBuildOptions {
    pub client: Option<CodexClient>,
    pub app_server: Option<AppServerClient>,
    pub skill_registry: Option<SkillRegistry>,
    pub surface_registry: SurfaceRegistry,
    pub compatibility_matrix: CompatibilityMatrix,
    pub job_registry: JobRegistry,
    pub state_store: StateStore,
    pub operation_mode: OperationMode,
    pub api_stability: ApiStability,
    pub delegation_policy: DelegationPolicy,
}

pub fn build_routes(
    options: RouteBuildOptions,
) -> impl Filter<Extract = impl Reply, Error = warp::Rejection> + Clone {
    let surface_registry = Arc::new(options.surface_registry);
    let state = AppState {
        client: options.client.map(Arc::new),
        app_server: options.app_server.map(Arc::new),
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

    health
        .or(models_v1)
        .or(models)
        .or(bridge_surfaces)
        .or(bridge_surface)
        .or(bridge_compatibility)
        .or(bridge_jobs)
        .or(bridge_session)
        .or(bridge_mode)
        .or(anthropic_v1)
        .or(anthropic)
        .or(openai_v1)
        .or(openai)
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
    match &state.client {
        Some(client) => client.list_models().await,
        None => vec!["gpt-5.2-codex".to_string()],
    }
}

async fn list_public_models(state: &AppState) -> Vec<String> {
    let mut models = Vec::new();

    if !matches!(state.operation_mode, OperationMode::ResponsesOnly) {
        if let Some(app_server) = &state.app_server {
            if let Ok(app_server_models) = app_server.model_list().await {
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

    // Short-circuit if upstream rate-limit is cached.
    if let Some(cached_body) = check_rate_limit_guard(&state).await {
        log::info!("[{trace_id}] rate-limit guard active, rejecting immediately");
        return Ok(
            warp::reply::with_status(
                warp::reply::json(&cached_body),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .into_response(),
        );
    }

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

    if let Some(response) = try_openai_local_command(&state, &request).await {
        return Ok(response);
    }

    if let Some(app_server) = state.app_server.as_ref() {
        if let Some(prompt) = build_openai_app_server_prompt(&request) {
            return match try_openai_via_app_server(
                app_server,
                &state,
                &request,
                prompt,
                response_bridge.clone(),
            )
            .await
            {
                Ok(response) => Ok(response),
                Err(err) if matches!(state.operation_mode, OperationMode::AutoHybrid) => {
                    log::warn!(
                        "[{trace_id}] app-server openai path failed, fallback to responses: {err}"
                    );
                    handle_openai_via_responses(
                        trace_id,
                        headers,
                        request,
                        state,
                        response_bridge,
                    )
                    .await
                }
                Err(err) => Ok(openai_error(
                    StatusCode::BAD_GATEWAY,
                    &format!("App-server request failed: {err}"),
                    "app_server_error",
                )),
            };
        }
    }

    handle_openai_via_responses(trace_id, headers, request, state, response_bridge).await
}

async fn handle_openai_via_responses(
    trace_id: String,
    _headers: warp::http::HeaderMap,
    request: ChatCompletionsRequest,
    state: AppState,
    bridge: Option<BridgeMetadata>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let Some(client) = state.client.as_ref() else {
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

    let response =
        match request_with_tool_fallback(client, codex_request, has_tools, &trace_id).await {
            Ok(v) => v,
            Err(UpstreamError::Upstream { status, body }) => {
                log::warn!(
                    "[{trace_id}] upstream failed ({status}): {}",
                    truncate(&body, 240)
                );
                
                if status == StatusCode::TOO_MANY_REQUESTS {
                    cache_rate_limit(&state, &body).await;
                }
                
                let reply_response = if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) {
                    warp::reply::with_status(warp::reply::json(&parsed), status).into_response()
                } else {
                    openai_error(status, "Upstream error", "upstream_error")
                };
                return Ok(reply_response);
            }
            Err(UpstreamError::Transport(e)) => {
                log::error!("[{trace_id}] transport error: {e}");
                return Ok(openai_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal proxy error",
                    "internal_error",
                ));
            }
        };

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

    // Short-circuit if upstream rate-limit is cached.
    if let Some(cached_body) = check_rate_limit_guard(&state).await {
        log::info!("[{trace_id}] rate-limit guard active, rejecting immediately");
        return Ok(
            warp::reply::with_status(
                warp::reply::json(&cached_body),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .into_response(),
        );
    }

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

    if let Some(response) = try_anthropic_local_command(&state, &request).await {
        return Ok(response);
    }

    if let Some(app_server) = state.app_server.as_ref() {
        if let Some((system_prompt, prompt)) = build_anthropic_app_server_prompt(&request) {
            return match try_anthropic_via_app_server(
                app_server,
                &state,
                &request,
                system_prompt,
                prompt,
                response_bridge.clone(),
            )
            .await
            {
                Ok(response) => Ok(response),
                Err(err) if matches!(state.operation_mode, OperationMode::AutoHybrid) => {
                    log::warn!("[{trace_id}] app-server anthropic path failed, fallback to responses: {err}");
                    handle_anthropic_via_responses(trace_id, request, state, response_bridge).await
                }
                Err(err) => Ok(anthropic_error(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    &format!("App-server request failed: {err}"),
                )),
            };
        }
    }

    handle_anthropic_via_responses(trace_id, request, state, response_bridge).await
}

async fn handle_anthropic_via_responses(
    trace_id: String,
    request: AnthropicMessagesRequest,
    state: AppState,
    bridge: Option<BridgeMetadata>,
) -> Result<warp::reply::Response, warp::Rejection> {
    let Some(client) = state.client.as_ref() else {
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

    let response =
        match request_with_tool_fallback(client, codex_request, has_tools, &trace_id).await {
            Ok(v) => v,
            Err(UpstreamError::Upstream { status, body }) => {
                log::warn!(
                    "[{trace_id}] upstream failed ({status}): {}",
                    truncate(&body, 240)
                );
                
                if status == StatusCode::TOO_MANY_REQUESTS {
                    cache_rate_limit(&state, &body).await;
                }
                
                let reply_response = if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) {
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
                log::error!("[{trace_id}] transport error: {e}");
                return Ok(anthropic_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "api_error",
                    "Internal proxy error",
                ));
            }
        };

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

async fn handle_bridge_jobs(state: AppState) -> Result<impl Reply, warp::Rejection> {
    Ok(warp::reply::json(&state.job_registry.list().await))
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
    Ok(warp::reply::json(&json!({
        "operationMode": state.operation_mode,
        "apiStability": state.api_stability,
        "delegationPolicy": state.delegation_policy,
        "appServerAvailable": state.app_server.is_some(),
        "responsesFallbackAvailable": state.client.is_some(),
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

enum AppServerTurnOutcome {
    Completed(Vec<crate::app_server::AppServerEvent>),
    Interrupted(ServerTurnInterruption),
}

struct ServerTurnInterruption {
    kind: &'static str,
    message: String,
    payload: serde_json::Value,
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
            Some(LocalCommandOutcome {
                surface_id: "command.security_review".to_string(),
                body: render_command_result(
                    &map_security_review_command(request, &state.job_registry).await,
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

fn latest_anthropic_user_text(request: &AnthropicMessagesRequest) -> Option<String> {
    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .and_then(|message| flatten_anthropic_content(&message.content))
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
    format!(
        "Proposed guidance bootstrap at `{}`.",
        result.proposed_path
    )
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

async fn wait_for_turn_outcome(
    thread_id: &str,
    turn_id: &str,
    mut notifications: broadcast::Receiver<crate::app_server::JsonRpcNotification>,
    mut server_requests: broadcast::Receiver<crate::app_server::JsonRpcRequest>,
) -> anyhow::Result<AppServerTurnOutcome> {
    let mut events = Vec::new();

    loop {
        tokio::select! {
            notification = notifications.recv() => {
                match notification {
                    Ok(notification) => {
                        let event = crate::app_server::AppServerEvent::from(notification);
                        if event.thread_id.as_deref() != Some(thread_id) {
                            continue;
                        }

                        if event.turn_id.as_deref() != Some(turn_id) && !matches!(event.kind, crate::app_server::AppServerEventKind::TurnCompleted | crate::app_server::AppServerEventKind::TurnStarted) {
                            continue;
                        }

                        let done = matches!(event.kind, crate::app_server::AppServerEventKind::TurnCompleted);
                        events.push(event);
                        if done {
                            return Ok(AppServerTurnOutcome::Completed(events));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        anyhow::bail!("app-server notification channel closed")
                    }
                }
            }
            request = server_requests.recv() => {
                match request {
                    Ok(request) => {
                        if request.params.get("threadId").and_then(|value| value.as_str()) != Some(thread_id) {
                            continue;
                        }
                        if request.params.get("turnId").and_then(|value| value.as_str()) != Some(turn_id) {
                            continue;
                        }
                        if let Some(interruption) = describe_server_request(&request) {
                            return Ok(AppServerTurnOutcome::Interrupted(interruption));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        anyhow::bail!("app-server server-request channel closed")
                    }
                }
            }
        }
    }
}

fn describe_server_request(
    request: &crate::app_server::JsonRpcRequest,
) -> Option<ServerTurnInterruption> {
    match request.method.as_str() {
        "item/commandExecution/requestApproval" => {
            let command = request
                .params
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or("command execution");
            let reason = request
                .params
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or("Approval is required before continuing.");
            Some(ServerTurnInterruption {
                kind: "approval_required",
                message: format!("{reason} Command: {command}"),
                payload: serde_json::json!({
                    "method": request.method,
                    "command": request.params.get("command").cloned(),
                    "cwd": request.params.get("cwd").cloned(),
                    "reason": request.params.get("reason").cloned(),
                    "itemId": request.params.get("itemId").cloned(),
                }),
            })
        }
        "item/fileChange/requestApproval" => {
            let reason = request
                .params
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or("File changes require approval before continuing.");
            Some(ServerTurnInterruption {
                kind: "approval_required",
                message: reason.to_string(),
                payload: serde_json::json!({
                    "method": request.method,
                    "grantRoot": request.params.get("grantRoot").cloned(),
                    "reason": request.params.get("reason").cloned(),
                    "itemId": request.params.get("itemId").cloned(),
                }),
            })
        }
        "item/tool/requestUserInput" => {
            let question = request
                .params
                .get("questions")
                .and_then(|value| value.as_array())
                .and_then(|questions| questions.first())
                .and_then(|question| question.get("question"))
                .and_then(|value| value.as_str())
                .unwrap_or("Additional user input is required.");
            Some(ServerTurnInterruption {
                kind: "clarification_required",
                message: question.to_string(),
                payload: serde_json::json!({
                    "method": request.method,
                    "questions": request.params.get("questions").cloned(),
                    "itemId": request.params.get("itemId").cloned(),
                }),
            })
        }
        _ => None,
    }
}

fn openai_turn_interrupted(interruption: &ServerTurnInterruption) -> warp::reply::Response {
    warp::reply::with_status(
        warp::reply::json(&json!({
            "error": {
                "message": interruption.message,
                "type": interruption.kind,
                "code": interruption.kind
            },
            "bridge": {
                "event": interruption.kind
            },
            "interruption": interruption.payload,
        })),
        StatusCode::CONFLICT,
    )
    .into_response()
}

fn anthropic_turn_interrupted(interruption: &ServerTurnInterruption) -> warp::reply::Response {
    warp::reply::with_status(
        warp::reply::json(&json!({
            "type": "error",
            "error": {
                "type": interruption.kind,
                "message": interruption.message,
            },
            "bridge": {
                "event": interruption.kind
            },
            "interruption": interruption.payload,
        })),
        StatusCode::CONFLICT,
    )
    .into_response()
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
async fn cache_rate_limit(state: &AppState, body: &str) {
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

    // Cap the cache TTL at 10 minutes to avoid stale entries from clock skew.
    let ttl = Duration::from_secs(resets_in.min(600));
    let mut guard = state.rate_limit_guard.write().await;
    *guard = Some(CachedRateLimit {
        body: parsed,
        expires_at: std::time::Instant::now() + ttl,
    });
}

fn build_anthropic_app_server_prompt(
    request: &AnthropicMessagesRequest,
) -> Option<(Option<String>, String)> {
    if request.stream.unwrap_or(false) || request.tools.is_some() {
        return None;
    }

    let system_prompt = request.system.as_ref().and_then(flatten_anthropic_system);
    let mut messages = Vec::new();

    for message in &request.messages {
        let content = flatten_anthropic_content(&message.content)?;
        messages.push(format!("{}: {}", message.role, content));
    }

    Some((system_prompt, messages.join("\n\n")))
}

fn build_openai_app_server_prompt(request: &ChatCompletionsRequest) -> Option<String> {
    if request.stream.unwrap_or(false)
        || request.tools.is_some()
        || request.functions.is_some()
        || request
            .messages
            .iter()
            .any(|message| message.tool_calls.is_some() || message.function_call.is_some())
    {
        return None;
    }

    let mut messages = Vec::new();
    for message in &request.messages {
        let content = flatten_openai_content(message.content.as_ref()?)?;
        messages.push(format!("{}: {}", message.role, content));
    }

    Some(messages.join("\n\n"))
}

async fn try_anthropic_via_app_server(
    client: &AppServerClient,
    state: &AppState,
    request: &AnthropicMessagesRequest,
    system_prompt: Option<String>,
    prompt: String,
    bridge: Option<BridgeMetadata>,
) -> anyhow::Result<warp::reply::Response> {
    let cwd = std::env::current_dir()?.display().to_string();
    let resolved_model = resolve_model_profile(&request.model);
    let thread = client
        .thread_start(ThreadStartRequest {
            cwd: Some(cwd.clone()),
            approval_policy: Some(ApprovalPolicy::OnRequest),
            sandbox: Some(SandboxConfig::WorkspaceWrite),
            model: Some(resolved_model.backend_model.clone()),
            model_provider: None,
            developer_instructions: system_prompt,
            base_instructions: None,
            ephemeral: Some(true),
        })
        .await?;
    let notifications = client.subscribe_notifications();
    let server_requests = client.subscribe_server_requests();
    let reasoning_effort = effective_anthropic_reasoning_effort(request);
    let turn = client
        .turn_start(TurnStartRequest {
            thread_id: thread.thread_id.clone(),
            input: vec![UserInput::Text { text: prompt }],
            approval_policy: None,
            cwd: Some(thread.cwd.clone()),
            model: Some(resolved_model.backend_model),
            sandbox_policy: None,
            effort: reasoning_effort.clone(),
            summary: reasoning_effort.map(|_| "auto".to_string()),
        })
        .await?;
    store_session(state, &thread).await;
    let outcome = timeout(
        Duration::from_secs(30),
        wait_for_turn_outcome(
            &thread.thread_id,
            &turn.turn_id,
            notifications,
            server_requests,
        ),
    )
    .await??;
    let events = match outcome {
        AppServerTurnOutcome::Completed(events) => events,
        AppServerTurnOutcome::Interrupted(interruption) => {
            client.turn_interrupt(&thread.thread_id, &turn.turn_id).await.ok();
            return Ok(anthropic_turn_interrupted(&interruption));
        }
    };
    let text = collect_text_from_events(&events);
    let payload = AnthropicMessagesResponse {
        id: format!("msg_{}", Uuid::new_v4()),
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        model: request.model.clone(),
        content: vec![AnthropicResponseContentBlock::Text { text }],
        stop_reason: "end_turn".to_string(),
        stop_sequence: None,
        usage: AnthropicUsage {
            input_tokens: 0,
            output_tokens: 0,
        },
    };
    Ok(json_response_with_bridge(&payload, bridge.as_ref()))
}

async fn try_openai_via_app_server(
    client: &AppServerClient,
    state: &AppState,
    request: &ChatCompletionsRequest,
    prompt: String,
    bridge: Option<BridgeMetadata>,
) -> anyhow::Result<warp::reply::Response> {
    let cwd = std::env::current_dir()?.display().to_string();
    let resolved_model = resolve_model_profile(&request.model);
    let thread = client
        .thread_start(ThreadStartRequest {
            cwd: Some(cwd.clone()),
            approval_policy: Some(ApprovalPolicy::OnRequest),
            sandbox: Some(SandboxConfig::WorkspaceWrite),
            model: Some(resolved_model.backend_model.clone()),
            model_provider: None,
            developer_instructions: None,
            base_instructions: None,
            ephemeral: Some(true),
        })
        .await?;
    let notifications = client.subscribe_notifications();
    let server_requests = client.subscribe_server_requests();
    let reasoning_effort = effective_openai_reasoning_effort(request);
    let turn = client
        .turn_start(TurnStartRequest {
            thread_id: thread.thread_id.clone(),
            input: vec![UserInput::Text { text: prompt }],
            approval_policy: None,
            cwd: Some(thread.cwd.clone()),
            model: Some(resolved_model.backend_model),
            sandbox_policy: None,
            effort: reasoning_effort.clone(),
            summary: reasoning_effort.map(|_| "auto".to_string()),
        })
        .await?;
    store_session(state, &thread).await;
    let outcome = timeout(
        Duration::from_secs(30),
        wait_for_turn_outcome(
            &thread.thread_id,
            &turn.turn_id,
            notifications,
            server_requests,
        ),
    )
    .await??;
    let events = match outcome {
        AppServerTurnOutcome::Completed(events) => events,
        AppServerTurnOutcome::Interrupted(interruption) => {
            client.turn_interrupt(&thread.thread_id, &turn.turn_id).await.ok();
            return Ok(openai_turn_interrupted(&interruption));
        }
    };
    let text = collect_text_from_events(&events);
    let payload = ChatCompletionsResponse {
        id: format!("chatcmpl-{}", Uuid::new_v4()),
        object: "chat.completion".to_string(),
        created: chrono::Utc::now().timestamp(),
        model: request.model.clone(),
        choices: vec![OpenAIChoice {
            index: 0,
            message: OpenAIResponseMessage {
                role: "assistant".to_string(),
                content: Some(text),
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
    Ok(json_response_with_bridge(&payload, bridge.as_ref()))
}

async fn store_session(state: &AppState, thread: &crate::app_server::ThreadStartResult) {
    let previous = state
        .state_store
        .get_session(&thread.thread_id)
        .await;
    let sandbox_config = parse_sandbox_config(&thread.sandbox);
    let session = BridgeSession {
        bridge_session_id: thread.thread_id.clone(),
        claude_session_id: None,
        thread: BridgeThread {
            thread_id: thread.thread_id.clone(),
            bridge_session_id: thread.thread_id.clone(),
            cwd: thread.cwd.clone(),
            project_root: None,
            approval_policy: thread.approval_policy,
            sandbox_config,
            created_at_unix: thread.created_at,
            turn_count: previous
                .as_ref()
                .map(|session| session.thread.turn_count + 1)
                .unwrap_or(1),
        },
        transport: TransportKind::Stdio,
        operation_mode: state.operation_mode,
        api_stability: state.api_stability,
        delegation_policy: state.delegation_policy.clone(),
        active_guidance_layers: previous
            .as_ref()
            .map(|session| session.active_guidance_layers.clone())
            .unwrap_or_default(),
        active_skills: previous
            .as_ref()
            .map(|session| session.active_skills.clone())
            .unwrap_or_default(),
        active_jobs: previous
            .as_ref()
            .map(|session| session.active_jobs.clone())
            .unwrap_or_default(),
        state_version: 1,
    };
    state.state_store.insert_session(session).await;
}

fn collect_text_from_events(events: &[crate::app_server::AppServerEvent]) -> String {
    let mut text = String::new();
    for event in events {
        if let Some(delta) = &event.delta {
            text.push_str(delta);
        }
    }
    text
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
    use std::path::PathBuf;

    use super::*;
    use crate::domain::anthropic::{AnthropicContent, AnthropicMessage, AnthropicSystem};
    use crate::skills::load_skill_registry;
    use crate::surfaces::CompatibilityMatrix;
    use crate::surfaces::SurfaceRegistry;

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
            client: None,
            app_server: None,
            skill_registry: None,
            surface_registry: Arc::new(registry.clone()),
            compatibility_matrix: Arc::new(matrix),
            classifier: Arc::new(SurfaceClassifier::new(registry)),
            job_registry: JobRegistry::default(),
            state_store: StateStore::default(),
            operation_mode: OperationMode::AutoHybrid,
            api_stability: ApiStability::Stable,
            delegation_policy: DelegationPolicy::ExplicitOnly,
        };

        let response = dispatch_local_command(&state, "/tasks").await.expect("command");
        assert_eq!(response.surface_id, "command.tasks");
        assert!(response.body.contains("0 active jobs"));
    }

    #[tokio::test]
    async fn local_security_review_command_creates_job() {
        let registry = SurfaceRegistry::new();
        let matrix = CompatibilityMatrix::new(&registry);
        let state = AppState {
            client: None,
            app_server: None,
            skill_registry: None,
            surface_registry: Arc::new(registry.clone()),
            compatibility_matrix: Arc::new(matrix),
            classifier: Arc::new(SurfaceClassifier::new(registry)),
            job_registry: JobRegistry::default(),
            state_store: StateStore::default(),
            operation_mode: OperationMode::AutoHybrid,
            api_stability: ApiStability::Stable,
            delegation_policy: DelegationPolicy::ExplicitOnly,
        };

        let response = dispatch_local_command(&state, "/security-review src/")
            .await
            .expect("command");
        assert_eq!(response.surface_id, "command.security_review");
        assert!(response.body.contains("Security review started"));
        assert_eq!(state.job_registry.list().await.len(), 1);
    }
}
