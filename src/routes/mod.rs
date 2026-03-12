use std::sync::Arc;

use bytes::Bytes;
use serde_json::json;
use uuid::Uuid;
use warp::{http::StatusCode, Filter, Reply};

use crate::domain::anthropic::{AnthropicError, AnthropicErrorBody, AnthropicMessagesRequest};
use crate::domain::codex::CodexResponsesRequest;
use crate::domain::openai::ChatCompletionsRequest;
use crate::proxy::codex_client::{CodexClient, UpstreamError};
use crate::translation::anthropic_to_codex::translate_anthropic_to_codex;
use crate::translation::codex_to_anthropic::{
    collect_codex_to_anthropic, stream_codex_to_anthropic,
};
use crate::translation::codex_to_openai::{collect_codex_to_openai, stream_codex_to_openai};
use crate::translation::openai_to_codex::translate_openai_to_codex;

#[derive(Clone)]
struct AppState {
    client: Arc<CodexClient>,
}

pub fn build_routes(
    client: CodexClient,
) -> impl Filter<Extract = impl Reply, Error = warp::Rejection> + Clone {
    let state = AppState {
        client: Arc::new(client),
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
    let models = state.client.list_models().await;

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

    let codex_request = translate_openai_to_codex(&request);
    let has_tools = codex_request
        .tools
        .as_ref()
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    let response = match request_with_tool_fallback(
        &state.client,
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
            return Ok(openai_error(status, "Upstream error", "upstream_error"));
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
        let stream = stream_codex_to_openai(response, request.model, trace_id);
        let sse = warp::sse::reply(warp::sse::keep_alive().stream(stream));
        let sse = warp::reply::with_header(sse, "cache-control", "no-cache");
        let sse = warp::reply::with_header(sse, "x-accel-buffering", "no");
        return Ok(sse.into_response());
    }

    match collect_codex_to_openai(response, request.model).await {
        Ok(payload) => Ok(warp::reply::json(&payload).into_response()),
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

    let codex_request = translate_anthropic_to_codex(&request);
    let has_tools = codex_request
        .tools
        .as_ref()
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    let response = match request_with_tool_fallback(
        &state.client,
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
            return Ok(anthropic_error(
                StatusCode::BAD_GATEWAY,
                "api_error",
                "Upstream error",
            ));
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

    if request.stream.unwrap_or(false) {
        let stream = stream_codex_to_anthropic(response, request.model, trace_id);
        let sse = warp::sse::reply(warp::sse::keep_alive().stream(stream));
        let sse = warp::reply::with_header(sse, "cache-control", "no-cache");
        let sse = warp::reply::with_header(sse, "x-accel-buffering", "no");
        return Ok(sse.into_response());
    }

    match collect_codex_to_anthropic(response, request.model).await {
        Ok(payload) => Ok(warp::reply::json(&payload).into_response()),
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

async fn request_with_tool_fallback(
    client: &CodexClient,
    mut request: CodexResponsesRequest,
    has_tools: bool,
    trace_id: &str,
) -> std::result::Result<reqwest::Response, UpstreamError> {
    match client.create_response(&request).await {
        Ok(v) => Ok(v),
        Err(UpstreamError::Upstream { status: _, body })
            if has_tools && CodexClient::is_tool_unsupported(&body) =>
        {
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

fn truncate(v: &str, max: usize) -> String {
    if v.len() <= max {
        return v.to_string();
    }

    format!("{}...", &v[..max])
}
