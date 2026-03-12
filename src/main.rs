use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use warp::{Filter, Reply};
use reqwest::Client;
use uuid::Uuid;

mod improved_response;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Port to listen on
    #[arg(short, long, default_value = "8080")]
    port: u16,

    /// Path to Codex auth.json file
    #[arg(long, default_value = "~/.codex/auth.json")]
    auth_path: String,
}

/// Chat Completions API format (what CLINE sends)
#[derive(Deserialize, Debug, Clone)]
struct ChatCompletionsRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: Option<f32>,
    max_tokens: Option<i32>,
    stream: Option<bool>,
    tools: Option<Vec<Value>>,
    tool_choice: Option<Value>,
}

#[derive(Deserialize, Debug, Clone)]
struct ChatMessage {
    role: String,
    content: Value, // Can be string or array
}

/// Chat Completions API response format (what CLINE expects)
#[derive(Serialize, Debug, Clone)]
struct ChatCompletionsResponse {
    id: String,
    object: String,
    created: i64,
    model: String,
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Serialize, Debug, Clone)]
struct Choice {
    index: i32,
    message: ChatResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Serialize, Debug, Clone)]
struct ChatResponseMessage {
    role: String,
    content: String,
}

#[derive(Serialize, Debug, Clone)]
struct Usage {
    prompt_tokens: i32,
    completion_tokens: i32,
    total_tokens: i32,
}

/// Codex Responses API format (what we send to ChatGPT backend)
#[derive(Serialize, Debug, Clone)]
struct ResponsesApiRequest {
    model: String,
    instructions: String,
    input: Vec<ResponseItem>,
    tools: Vec<Value>,
    tool_choice: String,
    parallel_tool_calls: bool,
    reasoning: Option<Value>,
    store: bool,
    stream: bool,
    include: Vec<String>,
}

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseItem {
    Message {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        role: String,
        content: Vec<ContentItem>,
    },
}

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentItem {
    InputText { text: String },
    OutputText { text: String },
}

/// Codex auth.json structure
#[derive(Deserialize, Debug, Clone)]
struct AuthData {
    #[serde(rename = "OPENAI_API_KEY")]
    api_key: Option<String>,
    tokens: Option<TokenData>,
}

#[derive(Deserialize, Debug, Clone)]
struct TokenData {
    access_token: String,
    account_id: String,
    refresh_token: Option<String>,
}

/// Codex Responses API response format
#[derive(Deserialize, Debug)]
struct ResponsesApiResponse {
    response: Option<ResponseOutput>,
    id: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ResponseOutput {
    content: Option<Vec<ResponseContentItem>>,
    role: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ResponseContentItem {
    #[serde(rename = "type")]
    content_type: String,
    text: Option<String>,
}

/// Anthropic Messages API response format
#[derive(Serialize, Debug)]
struct AnthropicMessagesResponse {
    id: String,
    #[serde(rename = "type")]
    msg_type: String,
    role: String,
    model: String,
    content: Vec<AnthropicContentItem>,
    stop_reason: String,
    stop_sequence: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentItem {
    Text { text: String },
}

#[derive(Serialize, Debug)]
struct AnthropicUsage {
    input_tokens: i32,
    output_tokens: i32,
}

struct ProxyServer {
    client: Client,
    auth_data: AuthData,
}

impl ProxyServer {
    async fn new(auth_path: &str) -> Result<Self> {
        let auth_path = if auth_path.starts_with("~/") {
            let home = std::env::var("HOME").context("HOME environment variable not set")?;
            auth_path.replace("~", &home)
        } else {
            auth_path.to_string()
        };

        let auth_content = tokio::fs::read_to_string(&auth_path)
            .await
            .context("Failed to read auth.json")?;

        let auth_data: AuthData = serde_json::from_str(&auth_content)
            .context("Failed to parse auth.json")?;

        // Create client with browser-like configuration
        let client = Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            client,
            auth_data,
        })
    }

    fn convert_chat_to_responses(&self, chat_req: ChatCompletionsRequest) -> ResponsesApiRequest {
        let mut input = Vec::new();
        let mut system_instructions = Vec::new();
        
        for msg in chat_req.messages {
            // Convert content to string (handle both string and array formats)
            let content_text = match &msg.content {
                Value::String(s) => s.clone(),
                Value::Array(arr) => {
                    arr.iter()
                        .filter_map(|v| {
                            if let Some(obj) = v.as_object() {
                                obj.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                            } else {
                                v.as_str().map(|s| s.to_string())
                            }
                        })
                        .collect::<Vec<String>>()
                        .join(" ")
                },
                _ => msg.content.to_string(),
            };
            
            match msg.role.as_str() {
                "system" => {
                    // System messages go into instructions
                    system_instructions.push(content_text);
                },
                "user" => {
                    input.push(ResponseItem::Message {
                        id: None,
                        role: "user".to_string(),
                        content: vec![ContentItem::InputText { text: content_text }],
                    });
                },
                "assistant" => {
                    input.push(ResponseItem::Message {
                        id: None,
                        role: "assistant".to_string(),
                        content: vec![ContentItem::OutputText { text: content_text }],
                    });
                },
                // Skip tool_use, tool_result, and other unsupported roles
                _ => {
                    println!("⚠️ Skipping unsupported message role: {}", msg.role);
                }
            }
        }

        let instructions = if system_instructions.is_empty() {
            "You are a helpful AI assistant. Provide clear, accurate, and concise responses.".to_string()
        } else {
            system_instructions.join("\n\n")
        };

        ResponsesApiRequest {
            model: chat_req.model,
            instructions,
            input,
            tools: vec![],
            tool_choice: "none".to_string(),
            parallel_tool_calls: false,
            reasoning: None,
            store: false,
            stream: true,
            include: vec![],
        }
    }


    async fn proxy_request(&self, chat_req: ChatCompletionsRequest) -> Result<ChatCompletionsResponse> {
        // Convert to Responses API format
        let responses_req = self.convert_chat_to_responses(chat_req);
        
        // Build request to ChatGPT backend with browser-like headers
        let mut request_builder = self.client
            .post("https://chatgpt.com/backend-api/codex/responses")
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Accept-Encoding", "gzip, deflate, br")
            .header("Referer", "https://chatgpt.com/")
            .header("Origin", "https://chatgpt.com")
            .header("Sec-Fetch-Dest", "empty")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Site", "same-origin")
            .header("Cache-Control", "no-cache")
            .header("Pragma", "no-cache")
            .header("DNT", "1")
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "codex_cli_rs");

        // Add authentication
        if let Some(tokens) = &self.auth_data.tokens {
            request_builder = request_builder.header("Authorization", format!("Bearer {}", tokens.access_token));
            request_builder = request_builder.header("chatgpt-account-id", &tokens.account_id);
        } else if let Some(api_key) = &self.auth_data.api_key {
            request_builder = request_builder.header("Authorization", format!("Bearer {}", api_key));
        }

        // Add session ID
        let session_id = Uuid::new_v4();
        request_builder = request_builder.header("session_id", session_id.to_string());

        // Send request
        let response = request_builder
            .json(&responses_req)
            .send()
            .await
            .context("Failed to send request to ChatGPT backend")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            println!("❌ Backend error ({}): {}", status, body);
            
            return Err(anyhow::anyhow!("Backend error: {}", status));
        }

        // Parse the response.completed event which contains the full final answer
        // This is the LAST event in the SSE stream and contains the definitive response
        let mut response_content = String::new();
        let response_text = response.text().await?;
        let lines: Vec<&str> = response_text.lines().collect();
        
        for line in lines {
            if line.starts_with("data: ") {
                let json_data = &line[6..];
                if json_data == "[DONE]" { break; }
                
                if let Ok(event) = serde_json::from_str::<serde_json::Value>(json_data) {
                    let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    
                    if event_type == "response.completed" {
                        // Extract text from the completed response output
                        if let Some(output) = event.pointer("/response/output") {
                            if let Some(arr) = output.as_array() {
                                for item in arr {
                                    if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                                        for c in content {
                                            if let Some(text) = c.get("text").and_then(|v| v.as_str()) {
                                                response_content = text.to_string();
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Create Chat Completions response
        let chat_res = ChatCompletionsResponse {
            id: format!("chatcmpl-{}", Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp(),
            model: responses_req.model.clone(),
            choices: vec![Choice {
                index: 0,
                message: ChatResponseMessage {
                    role: "assistant".to_string(),
                    content: response_content,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            }),
        };
        
        Ok(chat_res)
    }

}

// Enhanced logging function
fn log_request(method: &warp::http::Method, path: &str, headers: &warp::http::HeaderMap) {
    let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.3f UTC");
    
    println!("\n🔍 === INTERCEPTED REQUEST ===");
    println!("⏰ Timestamp: {}", timestamp);
    println!("📥 Method: {}", method);
    println!("📍 Path: {}", path);
    
    // Log all headers with special attention to problematic ones
    println!("\n📋 Headers ({} total):", headers.len());
    for (name, value) in headers.iter() {
        let header_name = name.as_str().to_lowercase();
        let value_str = match value.to_str() {
            Ok(v) => v,
            Err(_) => "[INVALID UTF-8]"
        };
        
        // Highlight potential CLINE-specific headers
        if header_name.contains("user-agent") || header_name.contains("client") || header_name.contains("cline") {
            println!("  🎯 {}: {}", name, value_str);
        } else if header_name == "authorization" {
            println!("  🔐 {}: {}***", name, &value_str[..std::cmp::min(20, value_str.len())]);
        } else {
            println!("  📄 {}: {}", name, value_str);
        }
    }
    
    // Check for VS Code specific patterns
    let user_agent = headers.get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("none");
    
    if user_agent.to_lowercase().contains("vscode") {
        println!("🎯 DETECTED: VS Code client!");
    }
    if user_agent.to_lowercase().contains("cline") {
        println!("🎯 DETECTED: CLINE extension!");
    }
    
    println!("🔍 === END INTERCEPT ===\n");
}

// Removed catch_all_handler - using inline closure to avoid body consumption conflicts

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    println!("Initializing Codex OpenAI Proxy...");
    
    let proxy = ProxyServer::new(&args.auth_path).await?;
    println!("✓ Loaded authentication from {}", args.auth_path);

    // Health check endpoint (removed unused variable warning)
    let _health = warp::path("health")
        .and(warp::get())
        .map(|| {
            println!("💚 Health check requested");
            warp::reply::json(&json!({
                "status": "ok",
                "service": "codex-openai-proxy"
            }))
        });

    // Multiple endpoints for CLINE compatibility
    let proxy_filter = warp::any().map(move || proxy.clone());
    
    let _chat_completions_v1 = warp::path!("v1" / "chat" / "completions")
        .and(warp::post())
        .and(warp::header::headers_cloned())
        .and(warp::body::json())
        .and(proxy_filter.clone())
        .and_then(handle_chat_completions);
        
    let _chat_completions_direct = warp::path("chat")
        .and(warp::path("completions"))
        .and(warp::path::end())
        .and(warp::post())
        .and(warp::header::headers_cloned())
        .and(warp::body::json())
        .and(proxy_filter.clone())
        .and_then(handle_chat_completions);

    // Models endpoints
    let _models_v1 = warp::path!("v1" / "models")
        .and(warp::get())
        .and(warp::header::headers_cloned())
        .and_then(handle_models);
        
    let _models_direct = warp::path("models")
        .and(warp::get())
        .and(warp::header::headers_cloned())
        .and_then(handle_models);

    // CORS headers - allow all headers to fix CLINE issues
    let cors = warp::cors()
        .allow_any_origin()
        .allow_headers(vec!["authorization", "content-type", "accept", "accept-encoding", "x-stainless-arch", "x-stainless-lang", "x-stainless-os", "x-stainless-package-version", "x-stainless-retry-count", "x-stainless-runtime", "x-stainless-runtime-version", "x-stainless-timeout"])
        .allow_methods(vec!["GET", "POST", "PUT", "DELETE", "OPTIONS"]);

    let _chat_messages_v1 = warp::path!("v1" / "messages")
        .and(warp::post())
        .and(warp::header::headers_cloned())
        .and(warp::body::json())
        .and(proxy_filter.clone())
        .and_then(handle_chat_completions); // Reusing handler as format is similar enough for extraction

    // BULLETPROOF SOLUTION - Single universal handler (removed old catch_all)
    let universal_handler = warp::any()
        .and(warp::method())
        .and(warp::path::full())
        .and(warp::header::headers_cloned())
        .and(warp::body::bytes())
        .and(proxy_filter.clone())
        .and_then(universal_request_handler);

    let routes = universal_handler
        .with(cors)
        .with(warp::log("codex_proxy"));

    println!("🚀 Codex OpenAI Proxy listening on http://0.0.0.0:{}", args.port);
    println!("   Health check: http://localhost:{}/health", args.port);
    println!("   Chat endpoint: http://localhost:{}/v1/chat/completions", args.port);
    println!("\n   Configure CLINE with:");
    println!("   Base URL: http://localhost:{}", args.port);
    println!("   Model: gpt-5.4");
    println!("   API Key: (any value)");

    warp::serve(routes)
        .run(([0, 0, 0, 0], args.port))
        .await;

    Ok(())
}

// Universal handler that routes based on path and method
async fn universal_request_handler(
    method: warp::http::Method,
    path: warp::path::FullPath,
    headers: warp::http::HeaderMap,
    body: bytes::Bytes,
    proxy: ProxyServer,
) -> Result<impl warp::Reply, warp::Rejection> {
    let path_str = path.as_str();
    
    log_request(&method, path_str, &headers);
    
    match (method.as_str(), path_str) {
        ("GET", "/health") => {
            println!("💚 Health check requested");
            Ok(warp::reply::json(&json!({
                "status": "ok",
                "service": "codex-openai-proxy"
            })).into_response())
        },
        ("GET", "/models") | ("GET", "/v1/models") => {
            println!("📋 === MATCHED MODELS REQUEST ===");
            println!("📋 === END MATCHED ===\n");
            
            let models_response = json!({
                "object": "list",
                "data": [
                    {
                        "id": "gpt-4",
                        "object": "model",
                        "created": 1687882411,
                        "owned_by": "openai"
                    },
                    {
                        "id": "gpt-5",
                        "object": "model", 
                        "created": 1687882411,
                        "owned_by": "openai"
                    },
                    {
                        "id": "gpt-5.4",
                        "object": "model", 
                        "created": 1687882411,
                        "owned_by": "openai"
                    }
                ]
            });
            
            Ok(warp::reply::json(&models_response).into_response())
        },
        ("POST", "/chat/completions") | ("POST", "/v1/chat/completions") | ("POST", "/v1/messages") | ("POST", "/messages") => {
            println!("🔥 === MATCHED CHAT/MESSAGES REQUEST ===");
            
            // LOG EXACT CLINE REQUEST FOR CURL REPLICATION
            println!("\n📋 === CLINE REQUEST DETAILS FOR CURL ===");
            println!("Method: POST");
            println!("Path: {}", path_str);
            println!("Body size: {} bytes", body.len());
            
            // Log all headers in curl format
            println!("\nHeaders for curl:");
            for (name, value) in headers.iter() {
                if let Ok(value_str) = value.to_str() {
                    if name.as_str().to_lowercase() == "authorization" {
                        println!("  -H \"{}: {}***\"", name, &value_str[..std::cmp::min(20, value_str.len())]);
                    } else if name.as_str().to_lowercase().starts_with("x-forwarded") {
                        println!("  # Skip: -H \"{}: {}\"", name, value_str);
                    } else {
                        println!("  -H \"{}: {}\"", name, value_str);
                    }
                }
            }
            
            // Log body (truncated for readability)
            println!("\nBody (first 1000 chars):");
            if let Ok(body_str) = std::str::from_utf8(&body) {
                let truncated = if body_str.len() > 1000 {
                    format!("{}... [TRUNCATED]", &body_str[..1000])
                } else {
                    body_str.to_string()
                };
                println!("{}", truncated);
                
                // Generate curl command
                println!("\n🚀 CURL COMMAND TO REPLICATE:");
                println!("curl -X POST http://localhost:8888{} \\", path_str);
                for (name, value) in headers.iter() {
                    if let Ok(value_str) = value.to_str() {
                        if !name.as_str().to_lowercase().starts_with("x-forwarded") 
                           && name.as_str().to_lowercase() != "host" {
                            if name.as_str().to_lowercase() == "authorization" {
                                println!("  -H \"{}: test-key\" \\", name);
                            } else {
                                println!("  -H \"{}: {}\" \\", name, value_str);
                            }
                        }
                    }
                }
                println!("  -d '{}'", body_str.chars().take(500).collect::<String>());
            }
            println!("📋 === END CLINE REQUEST DETAILS ===\n");
            
            // Parse JSON from bytes
            let mut chat_req: ChatCompletionsRequest = match serde_json::from_slice(&body) {
                Ok(req) => req,
                Err(e) => {
                    println!("❌ JSON parse error: {}", e);
                    return Ok(warp::reply::with_status(
                        "Invalid JSON",
                        warp::http::StatusCode::BAD_REQUEST
                    ).into_response());
                }
            };
            
            let is_anthropic = path_str.contains("/messages");
            
            // Fix Anthropic tool formats
            if is_anthropic {
                if let Some(tools) = &mut chat_req.tools {
                    for tool in tools.iter_mut() {
                        if let Some(obj) = tool.as_object_mut() {
                            if obj.contains_key("input_schema") && !obj.contains_key("type") {
                                let name = obj.remove("name").unwrap_or(json!(""));
                                let description = obj.remove("description").unwrap_or(json!(""));
                                let parameters = obj.remove("input_schema").unwrap_or(json!({}));
                                
                                *tool = json!({
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "description": description,
                                        "parameters": parameters
                                    }
                                });
                            }
                        }
                    }
                }
                
                // Fix tool_choice if it's sent as an object
                if let Some(tc) = &chat_req.tool_choice {
                    if let Some(obj) = tc.as_object() {
                        if let Some(t) = obj.get("type").and_then(|v| v.as_str()) {
                            if t == "auto" {
                                chat_req.tool_choice = Some(json!("auto"));
                            }
                        }
                    }
                }
            }
            
            println!("   Model: {}", chat_req.model);
            println!("   Messages: {} items", chat_req.messages.len());
            for (i, msg) in chat_req.messages.iter().enumerate() {
                let content_preview = match &msg.content {
                    Value::String(s) => s.chars().take(50).collect::<String>(),
                    Value::Array(arr) => format!("[array with {} items]", arr.len()),
                    _ => format!("[{}]", msg.content.to_string().chars().take(50).collect::<String>()),
                };
                println!("   [{}] {}: {}", i, msg.role, content_preview);
            }
            println!("🔥 === END MATCHED ===\n");
            
            // Check if streaming is requested
            
            if chat_req.stream.unwrap_or(false) {
                println!("🔄 STREAMING: Processing real backend request for {:?} format", if is_anthropic { "Anthropic" } else { "OpenAI" });
                
                // For now, get the full response and stream it as a single block to ensure accuracy
                match proxy.proxy_request(chat_req.clone()).await {
                    Ok(response) => {
                        let message = response.choices[0].message.content.clone();
                        let chunk_id = if is_anthropic { format!("msg_{}", Uuid::new_v4()) } else { format!("chatcmpl-{}", Uuid::new_v4()) };
                        let model = chat_req.model.clone();
                        
                        let sse_response = if is_anthropic {
                            // Anthropic format SSE
                            vec![
                                format!("event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"{}\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"{}\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{{\"input_tokens\":50,\"output_tokens\":1}}}}}}\n\n", chunk_id, model),
                                format!("event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n"),
                                format!("event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{}\"}}}}\n\n", message),
                                format!("event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n"),
                                format!("event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\",\"stop_sequence\":null}},\"usage\":{{\"output_tokens\":30}}}}\n\n"),
                                format!("event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"),
                            ].join("")
                        } else {
                            // OpenAI format SSE
                            vec![
                                format!("data: {{\"id\":\"{}\",\"object\":\"chat.completion.chunk\",\"created\":{},\"model\":\"{}\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\"}},\"finish_reason\":null}}]}}\n\n", 
                                        chunk_id, chrono::Utc::now().timestamp(), model),
                                format!("data: {{\"id\":\"{}\",\"object\":\"chat.completion.chunk\",\"created\":{},\"model\":\"{}\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{}\"}},\"finish_reason\":null}}]}}\n\n", 
                                        chunk_id, chrono::Utc::now().timestamp(), model, message),
                                format!("data: {{\"id\":\"{}\",\"object\":\"chat.completion.chunk\",\"created\":{},\"model\":\"{}\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n", 
                                        chunk_id, chrono::Utc::now().timestamp(), model),
                                "data: [DONE]\n\n".to_string(),
                            ].join("")
                        };
                        
                        let reply = warp::reply::with_header(sse_response, "content-type", "text/event-stream");
                        let reply = warp::reply::with_header(reply, "cache-control", "no-cache");
                        let reply = warp::reply::with_header(reply, "connection", "keep-alive");
                        let reply = warp::reply::with_header(reply, "access-control-allow-origin", "*");
                        Ok(reply.into_response())
                    },
                    Err(e) => {
                        eprintln!("Streaming proxy error: {:#}", e);
                        Ok(warp::reply::with_status("Backend error", warp::http::StatusCode::INTERNAL_SERVER_ERROR).into_response())
                    }
                }
            } else {
                match proxy.proxy_request(chat_req).await {
                    Ok(response) => {
                        if is_anthropic {
                            // Convert OpenAI response to Anthropic response
                            let anthropic_res = AnthropicMessagesResponse {
                                id: response.id,
                                msg_type: "message".to_string(),
                                role: "assistant".to_string(),
                                model: response.model,
                                content: vec![AnthropicContentItem::Text { 
                                    text: response.choices[0].message.content.clone() 
                                }],
                                stop_reason: "end_turn".to_string(),
                                stop_sequence: None,
                                usage: AnthropicUsage {
                                    input_tokens: response.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                                    output_tokens: response.usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
                                },
                            };
                            let reply = warp::reply::json(&anthropic_res);
                            let reply = warp::reply::with_header(reply, "content-type", "application/json");
                            let reply = warp::reply::with_header(reply, "access-control-allow-origin", "*");
                            Ok(reply.into_response())
                        } else {
                            let reply = warp::reply::json(&response);
                            let reply = warp::reply::with_header(reply, "content-type", "application/json");
                            let reply = warp::reply::with_header(reply, "access-control-allow-origin", "*");
                            Ok(reply.into_response())
                        }
                    },
                    Err(e) => {
                        eprintln!("Proxy error: {:#}", e);
                        let reply = warp::reply::json(&json!({
                            "error": {
                                "message": format!("Proxy error: {}", e),
                                "type": "proxy_error",
                                "code": "internal_error"
                            }
                        }));
                        let reply = warp::reply::with_header(reply, "content-type", "application/json");
                        let reply = warp::reply::with_header(reply, "access-control-allow-origin", "*");
                        Ok(reply.into_response())
                    }
                }
            }
        },
        _ => {
            println!("❌ UNMATCHED: {} {}", method, path_str);
            Ok(warp::reply::with_status(
                "Not found",
                warp::http::StatusCode::NOT_FOUND
            ).into_response())
        }
    }
}

async fn handle_models(
    headers: warp::http::HeaderMap,
) -> Result<impl warp::Reply, warp::Rejection> {
    log_request(&warp::http::Method::GET, "/models", &headers);
    
    println!("📋 === MATCHED MODELS REQUEST ===");
    println!("📋 === END MATCHED ===\n");
    
    // Return a simple models list for CLINE
    let models_response = json!({
        "object": "list",
        "data": [
            {
                "id": "gpt-4",
                "object": "model",
                "created": 1687882411,
                "owned_by": "openai"
            },
            {
                "id": "gpt-5",
                "object": "model", 
                "created": 1687882411,
                "owned_by": "openai"
            },
            {
                "id": "gpt-5.4",
                "object": "model", 
                "created": 1687882411,
                "owned_by": "openai"
            }
        ]
    });
    
    Ok(warp::reply::json(&models_response))
}

async fn handle_chat_completions(
    headers: warp::http::HeaderMap,
    req: ChatCompletionsRequest,
    proxy: ProxyServer,
) -> Result<impl warp::Reply, warp::Rejection> {
    // Enhanced logging for successful matches
    log_request(&warp::http::Method::POST, "/chat/completions", &headers);
    
    println!("🔥 === MATCHED CHAT COMPLETIONS ===");
    println!("   Model: {}", req.model);
    println!("   Messages: {} items", req.messages.len());
    println!("🔥 === END MATCHED ===\n");
    
    match proxy.proxy_request(req).await {
        Ok(response) => Ok(warp::reply::json(&response)),
        Err(e) => {
            eprintln!("Proxy error: {:#}", e);
            Ok(warp::reply::json(&json!({
                "error": {
                    "message": format!("Proxy error: {}", e),
                    "type": "proxy_error",
                    "code": "internal_error"
                }
            })))
        }
    }
}

// Make ProxyServer cloneable for warp filters
impl Clone for ProxyServer {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            auth_data: self.auth_data.clone(),
        }
    }
}